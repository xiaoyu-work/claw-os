//! Closed CopyQ operations. Only the broker sees the owner endpoint; the fixed
//! CLI connects through a private, credential-checked relay to that pinned peer.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::{authorize_session, ClientIdentity, Decision};
use crate::caps::{Cap, Scope, Verb};
use crate::clawd::authority::Authorized;
use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::bounded::{Text, Token};

const COPYQ: &str = "/usr/bin/copyq";
const MAX_CALL: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_ITEMS: usize = 20;
const PREVIEW_CHARS: usize = 180;

const LIST: &str = r#"
var request = JSON.parse(str(input()));
function checkDeadline() { if (Date.now() >= request.deadline_unix_ms) { print('deadline-expired'); abort(); } }
checkDeadline();
var name = str(config('clipboard_tab'));
if (!name) { print(JSON.stringify({error:'no-clipboard-tab'})); abort(); }
tab(name);
var result = [], previous = '';
for (var index = 0, count = size(); index < count && result.length < 20; index++) {
    checkDeadline();
    var value = getItem(index);
    if (!value || !(mimeText in value)) continue;
    var data = value[mimeText], text = str(data);
    if (!text || text.indexOf('\0') >= 0) continue;
    var digest = str(toBase64(sha256sum(data)));
    if (digest === previous) continue;
    previous = digest;
    var record = {identity:str(toBase64(sha256sum(pack(value)))), text:text};
    var timeType = 'application/x-copyq-user-copy-time';
    if (timeType in value) record.copied_at = str(value[timeType]);
    result.push(record);
}
print(JSON.stringify(result));
"#;
const RESTORE: &str = r#"
var request = JSON.parse(str(input()));
function checkDeadline() { if (Date.now() >= request.deadline_unix_ms) { print('deadline-expired'); abort(); } }
checkDeadline();
var wanted = request.identity, name = str(config('clipboard_tab'));
if (!name) { print('no-clipboard-tab'); abort(); }
tab(name);
var result = 'not-found';
for (var index = 0, count = size(); index < count; index++) {
    checkDeadline();
    var value = getItem(index);
    if (value && mimeText in value && str(toBase64(sha256sum(pack(value)))) === wanted) {
        checkDeadline(); select(index); result = 'ok'; break;
    }
}
print(result);
"#;
const REMOVE: &str = r#"
var request = JSON.parse(str(input()));
function checkDeadline() { if (Date.now() >= request.deadline_unix_ms) { print('deadline-expired'); abort(); } }
checkDeadline();
var wanted = request.identity, name = str(config('clipboard_tab'));
if (!name) { print('no-clipboard-tab'); abort(); }
tab(name);
var result = 'not-found';
for (var index = 0, count = size(); index < count; index++) {
    checkDeadline();
    var value = getItem(index);
    if (value && mimeText in value && str(toBase64(sha256sum(pack(value)))) === wanted) {
        checkDeadline(); remove(index); result = 'ok'; break;
    }
}
print(result);
"#;
const CLEAR: &str = r#"
var request = JSON.parse(str(input()));
function checkDeadline() { if (Date.now() >= request.deadline_unix_ms) { print('deadline-expired'); abort(); } }
checkDeadline();
var name = str(config('clipboard_tab'));
if (!name) { print('no-clipboard-tab'); abort(); }
tab(name);
while (size() > 0) { checkDeadline(); remove(size() - 1); }
print('ok');
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(super) enum Action {
    #[serde(rename = "history-list")]
    List,
    #[serde(rename = "history-restore")]
    Restore,
    #[serde(rename = "history-remove")]
    Remove,
    #[serde(rename = "history-clear")]
    Clear,
}

impl Action {
    fn script(self) -> &'static str {
        match self {
            Self::List => LIST,
            Self::Restore => RESTORE,
            Self::Remove => REMOVE,
            Self::Clear => CLEAR,
        }
    }

    fn needs(self) -> Vec<Cap> {
        let read = Cap::new(Verb::CLIPBOARD_READ, Scope::name("history"));
        let write = Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("history"));
        match self {
            Self::List => vec![read],
            Self::Restore => vec![
                read,
                write,
                Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("selection")),
            ],
            Self::Remove => vec![read, write],
            Self::Clear => vec![write],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    session: Token,
    action: Action,
    #[serde(default)]
    identity: Option<Text<64>>,
    #[serde(default)]
    confirm: bool,
    deadline_unix_ms: u64,
}

pub(super) fn validate(params: &Value) -> Result<Request, String> {
    let mut request: Request = serde_json::from_value(params.clone())
        .map_err(|_| "invalid closed clipboard history request")?;
    if request.session.as_str().is_empty() {
        return Err("clipboard history requires a session".into());
    }
    match request.action {
        Action::List if request.identity.is_none() && !request.confirm => {}
        Action::Clear if request.identity.is_none() && request.confirm => {}
        Action::Restore | Action::Remove
            if !request.confirm
                && request
                    .identity
                    .as_ref()
                    .is_some_and(|value| valid_identity(value.as_str())) => {}
        _ => return Err("invalid arguments for clipboard history action".into()),
    }
    let left = remaining(request.deadline_unix_ms)?;
    request.deadline_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "invalid system clock")?
        .as_millis() as u64
        + left.as_millis() as u64;
    Ok(request)
}

fn valid_identity(value: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .is_ok_and(|bytes| {
            bytes.len() == 32 && base64::engine::general_purpose::STANDARD.encode(bytes) == value
        })
}

fn remaining(deadline: u64) -> Result<Duration, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "invalid system clock")?
        .as_millis();
    let left = u128::from(deadline)
        .checked_sub(now)
        .filter(|left| *left > 0)
        .ok_or("clipboard history deadline expired")?;
    Ok(Duration::from_millis(left.min(MAX_CALL.as_millis()) as u64))
}

pub(super) async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request = validate(&params).map_err(BrokerError::execution)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    check_owner(authority, owner, &request)?;
    let result = async {
        if unsafe { libc::geteuid() } != 0 || owner == 0 {
            return Err(BrokerError::unavailable(
                "clipboard history requires root clawd and a non-root owner",
            ));
        }
        let home = crate::paths::verified_home_for_uid(owner).map_err(BrokerError::unavailable)?;
        let gid = crate::clawd::client_identity::owner_groups(owner)
            .map_err(BrokerError::unavailable)?
            .0;
        let executable = Executable::open(Path::new(COPYQ)).map_err(BrokerError::unavailable)?;
        let endpoint = endpoint(&home, owner).map_err(BrokerError::unavailable)?;
        execute(
            &request,
            owner,
            gid,
            authority,
            &executable,
            &endpoint,
            Path::new("/run/cos"),
        )
        .await
    }
    .await;
    authorized_error(result, authority, owner, request.action)
}

fn authorized_error(
    result: Result<Value, BrokerError>,
    authority: &Decision,
    owner: u32,
    action: Action,
) -> Result<Value, BrokerError> {
    // Error metadata also owes the route's capability spend. Otherwise the
    // mandatory middleware masks an unavailable service as a permission denial.
    if result.is_err() && !crate::clawd::authority::obligation_met(Some(authority)) {
        let _authorized = authorize_session(authority, owner, &action.needs())
            .map_err(BrokerError::authorization)?;
    }
    result
}

fn check_owner(authority: &Decision, owner: u32, request: &Request) -> Result<(), BrokerError> {
    if authority.owner_uid() != owner || authority.session_id() != Some(request.session.as_str()) {
        return Err(BrokerError::authorization(
            "clipboard history authority belongs to another owner/session",
        ));
    }
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    let needs = request.action.needs();
    if needs.iter().any(|cap| !authority.caps().covers(cap)) {
        return authorize_session(authority, owner, &needs)
            .map(drop)
            .map_err(BrokerError::authorization);
    }
    Ok(())
}

struct Executable {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl Executable {
    fn open(path: &Path) -> Result<Self, String> {
        crate::provenance::fsec::require_secure_location(path, &[0])
            .map_err(|_| "CopyQ executable is not root-owned and protected")?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "CopyQ is not installed or is unavailable")?;
        let metadata = file
            .metadata()
            .map_err(|_| "CopyQ executable is unavailable")?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o111 == 0
            || metadata.mode() & 0o022 != 0
        {
            return Err("CopyQ executable is not a protected system executable".into());
        }
        Ok(Self {
            path: path.into(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }

    fn process(&self, owner: u32, pid: u32) -> Result<u64, String> {
        let start =
            crate::proc::read_start_time_ticks_pub(pid).ok_or("CopyQ process disappeared")?;
        let status = fs::read_to_string(format!("/proc/{pid}/status"))
            .map_err(|_| "CopyQ process is unavailable")?;
        let ids = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .ok_or("CopyQ process owner is unavailable")?
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid CopyQ process owner")?;
        let path = format!("/proc/{pid}/exe");
        let metadata =
            fs::metadata(&path).map_err(|_| "CopyQ process executable is unavailable")?;
        if ids.len() != 4
            || ids.iter().any(|uid| *uid != owner)
            || (metadata.dev(), metadata.ino()) != (self.dev, self.ino)
            || fs::read_link(path).map_err(|_| "CopyQ executable is unavailable")? != self.path
            || crate::proc::read_start_time_ticks_pub(pid) != Some(start)
        {
            return Err(
                "clipboard history endpoint is not the owner's installed CopyQ process".into(),
            );
        }
        Ok(start)
    }
}

fn endpoint(home: &Path, owner: u32) -> Result<File, String> {
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home)
        .map_err(|_| "clipboard history owner home is unavailable")?;
    for name in [None, Some(".config"), Some("copyq")] {
        if let Some(name) = name {
            directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(format!("/proc/self/fd/{}/{name}", directory.as_raw_fd()))
                .map_err(|_| "CopyQ default history session is unavailable")?;
        }
        let metadata = directory
            .metadata()
            .map_err(|_| "CopyQ session directory is unavailable")?;
        if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
            return Err("CopyQ session directory is not owner-bound and protected".into());
        }
    }
    Ok(directory)
}

struct Proxy {
    root: PathBuf,
    listener: UnixListener,
}

impl Proxy {
    fn bind(parent: &Path, owner: u32) -> Result<Self, String> {
        crate::provenance::fsec::require_secure_location(parent, &[0])
            .map_err(|_| "clipboard history broker runtime is unavailable")?;
        let root = parent.join(format!(
            "ch-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        ));
        fs::create_dir(&root)
            .map_err(|error| format!("prepare clipboard history relay: {error}"))?;
        let result = (|| {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o711))
                .map_err(|error| error.to_string())?;
            let directory = root.join("copyq");
            fs::create_dir(&directory).map_err(|error| error.to_string())?;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o711))
                .map_err(|error| error.to_string())?;
            let path = directory.join(".copyq_s");
            let listener = UnixListener::bind(&path).map_err(|error| error.to_string())?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let path =
                CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
            if unsafe { libc::chown(path.as_ptr(), owner, u32::MAX) } != 0 {
                return Err("set clipboard history relay owner failed".to_string());
            }
            Ok(listener)
        })();
        match result {
            Ok(listener) => Ok(Self { root, listener }),
            Err(error) => {
                let _ = fs::remove_dir_all(&root);
                Err(error)
            }
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn bounded(pipe: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    pipe.take((limit + 1) as u64)
        .read_to_end(&mut output)
        .await
        .map_err(|error| error.to_string())?;
    if output.len() > limit {
        return Err("CopyQ response exceeds its byte limit".into());
    }
    Ok(output)
}

async fn relay(
    _authorized: &Authorized,
    left: UnixStream,
    right: UnixStream,
) -> Result<(), String> {
    let (left_read, left_write) = left.into_split();
    let (right_read, right_write) = right.into_split();
    let transfer = |reader: tokio::net::unix::OwnedReadHalf,
                    mut writer: tokio::net::unix::OwnedWriteHalf| async move {
        let size = tokio::io::copy(
            &mut reader.take((MAX_OUTPUT + 64 * 1024 + 1) as u64),
            &mut writer,
        )
        .await
        .map_err(|error| error.to_string())?;
        if size > (MAX_OUTPUT + 64 * 1024) as u64 {
            return Err("CopyQ transport exceeds its byte limit".into());
        }
        writer.shutdown().await.map_err(|error| error.to_string())
    };
    tokio::try_join!(
        transfer(left_read, right_write),
        transfer(right_read, left_write)
    )
    .map(|_| ())
}

async fn execute(
    request: &Request,
    owner: u32,
    gid: u32,
    authority: &Decision,
    executable: &Executable,
    directory: &File,
    runtime: &Path,
) -> Result<Value, BrokerError> {
    check_owner(authority, owner, request)?;
    let timeout = remaining(request.deadline_unix_ms).map_err(BrokerError::execution)?;
    let lock = super::CLIPBOARD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = tokio::time::timeout(timeout, lock.lock())
        .await
        .map_err(|_| BrokerError::unavailable("clipboard history is busy"))?;
    let proxy = Proxy::bind(runtime, owner).map_err(BrokerError::unavailable)?;
    let path = PathBuf::from(format!("/proc/self/fd/{}/.copyq_s", directory.as_raw_fd()));
    let before = fs::symlink_metadata(&path)
        .map_err(|_| BrokerError::unavailable("CopyQ history service is unavailable"))?;
    if !before.file_type().is_socket() || before.uid() != owner {
        return Err(BrokerError::unavailable(
            "CopyQ history endpoint is not an owner socket",
        ));
    }
    let mut dispatched = false;
    let result = tokio::time::timeout(
        remaining(request.deadline_unix_ms).map_err(BrokerError::execution)?,
        async {
            let remote = UnixStream::connect(&path)
                .await
                .map_err(|_| BrokerError::unavailable("CopyQ history service is unavailable"))?;
            let peer = remote
                .peer_cred()
                .map_err(|_| BrokerError::unavailable("CopyQ peer is unavailable"))?;
            let pid = peer
                .pid()
                .and_then(|pid| u32::try_from(pid).ok())
                .ok_or_else(|| BrokerError::unavailable("CopyQ peer PID is unavailable"))?;
            if peer.uid() != owner {
                return Err(BrokerError::unavailable("CopyQ peer owner mismatch"));
            }
            let start = executable
                .process(owner, pid)
                .map_err(BrokerError::unavailable)?;
            let after = fs::symlink_metadata(&path)
                .map_err(|_| BrokerError::unavailable("CopyQ endpoint disappeared"))?;
            if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
                return Err(BrokerError::unavailable("CopyQ endpoint was replaced"));
            }
            let mut command = std::process::Command::new(&executable.path);
            command
                .args(["eval", request.action.script()])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LC_ALL", "C.UTF-8")
                .env("HOME", "/")
                .env("XDG_CONFIG_HOME", &proxy.root)
                .env("QT_QPA_PLATFORM", "offscreen")
                .current_dir("/")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let parent = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(gid) != 0
                        || libc::setuid(owner) != 0
                        || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                        || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                        || libc::getppid() != parent
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    let limit = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::setrlimit(libc::RLIMIT_CORE as _, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = tokio::process::Command::from(command)
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| BrokerError::unavailable("CopyQ command service is unavailable"))?;
            let child_pid = child
                .id()
                .ok_or_else(|| BrokerError::unavailable("CopyQ command PID is unavailable"))?;
            let output = child
                .stdout
                .take()
                .ok_or_else(|| BrokerError::execution("CopyQ stdout unavailable"))?;
            let errors = child
                .stderr
                .take()
                .ok_or_else(|| BrokerError::execution("CopyQ stderr unavailable"))?;
            let mut input = child
                .stdin
                .take()
                .ok_or_else(|| BrokerError::execution("CopyQ stdin unavailable"))?;
            let operation = async {
                let payload = json!({
                    "identity": request.identity.as_ref().map(Text::as_str),
                    "deadline_unix_ms": request.deadline_unix_ms,
                }).to_string();
                input.write_all(payload.as_bytes()).await
                    .map_err(|error| BrokerError::execution(error.to_string()))?;
                input
                    .shutdown()
                    .await
                    .map_err(|error| BrokerError::execution(error.to_string()))?;
                drop(input);
                let permission = async {
                    let (local, _) = proxy
                        .listener
                        .accept()
                        .await
                        .map_err(|error| BrokerError::execution(error.to_string()))?;
                    let credentials = local
                        .peer_cred()
                        .map_err(|error| BrokerError::execution(error.to_string()))?;
                    if credentials.uid() != owner
                        || credentials.pid() != i32::try_from(child_pid).ok()
                    {
                        return Err(BrokerError::authorization(
                            "clipboard history relay caller mismatch",
                        ));
                    }
                    executable
                        .process(owner, child_pid)
                        .map_err(BrokerError::unavailable)?;
                    if executable
                        .process(owner, pid)
                        .map_err(BrokerError::unavailable)?
                        != start
                    {
                        return Err(BrokerError::unavailable(
                            "CopyQ provider process was replaced",
                        ));
                    }
                    remaining(request.deadline_unix_ms).map_err(BrokerError::execution)?;
                    let proof = authorize_session(authority, owner, &request.action.needs())
                        .map_err(BrokerError::authorization)?;
                    dispatched = true;
                    relay(&proof, local, remote)
                        .await
                        .map_err(BrokerError::execution)
                };
                let (stdout, _, _) = tokio::try_join!(
                    async {
                        bounded(output, MAX_OUTPUT)
                            .await
                            .map_err(BrokerError::execution)
                    },
                    async { bounded(errors, 8192).await.map_err(BrokerError::execution) },
                    permission,
                )?;
                let status = child
                    .wait()
                    .await
                    .map_err(|error| BrokerError::execution(error.to_string()))?;
                if stdout == b"deadline-expired\n" || stdout == b"deadline-expired" {
                    return Err(if request.action == Action::List {
                        BrokerError::unavailable("CopyQ history deadline expired")
                    } else {
                        BrokerError::indeterminate("CopyQ history deadline expired; inspect state before retrying")
                    });
                }
                if !status.success() {
                    return Err(if request.action == Action::List {
                        BrokerError::unavailable("CopyQ service could not complete the request")
                    } else {
                        BrokerError::indeterminate("CopyQ service did not confirm the action; inspect state before retrying")
                    });
                }
                parse(request.action, &stdout).map_err(BrokerError::execution)
            }
            .await;
            if operation.is_err() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            operation
        },
    )
    .await;
    result.map_err(|_| {
        if request.action == Action::List || !dispatched {
            BrokerError::unavailable("CopyQ history service did not respond before its deadline")
        } else {
            BrokerError::indeterminate(
                "CopyQ history deadline expired; inspect state before retrying",
            )
        }
    })?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    identity: String,
    text: String,
    #[serde(default)]
    copied_at: Option<String>,
}

#[derive(Serialize)]
struct Entry {
    identity: String,
    preview: String,
    copied_at: Option<String>,
}

fn parse(action: Action, raw: &[u8]) -> Result<Value, String> {
    if raw.len() > MAX_OUTPUT {
        return Err("CopyQ response exceeds its byte limit".into());
    }
    if action != Action::List {
        return match std::str::from_utf8(raw).map(str::trim) {
            Ok("ok") => Ok(match action {
                Action::Restore => json!({"restored":true}),
                Action::Remove => json!({"removed":true}),
                Action::Clear => json!({"cleared":true}),
                Action::List => unreachable!(),
            }),
            Ok("not-found") => {
                Err("clipboard history item changed or disappeared; refresh before retrying".into())
            }
            Ok("no-clipboard-tab") => Err("CopyQ has no configured clipboard history tab".into()),
            _ => Err("CopyQ returned an invalid history action result".into()),
        };
    }
    if serde_json::from_slice::<Value>(raw)
        .ok()
        .is_some_and(|value| value.get("error").and_then(Value::as_str) == Some("no-clipboard-tab"))
    {
        return Err("CopyQ has no configured clipboard history tab".into());
    }
    let values: Vec<RawEntry> =
        serde_json::from_slice(raw).map_err(|_| "CopyQ returned an invalid history snapshot")?;
    let mut entries: Vec<Entry> = Vec::new();
    for value in values {
        if !valid_identity(&value.identity) {
            return Err("CopyQ returned an invalid history identity".into());
        }
        if value.text.is_empty()
            || value.text.contains('\0')
            || entries
                .last()
                .is_some_and(|last| last.identity == value.identity)
        {
            continue;
        }
        let normalized = value.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut chars = normalized.chars();
        let mut preview: String = chars.by_ref().take(PREVIEW_CHARS).collect();
        if chars.next().is_some() {
            preview.push('…');
        }
        entries.push(Entry {
            identity: value.identity,
            preview,
            copied_at: value
                .copied_at
                .map(|value| value.trim().chars().take(256).collect::<String>())
                .filter(|value| !value.is_empty()),
        });
        if entries.len() == MAX_ITEMS {
            break;
        }
    }
    Ok(json!({"entries": entries}))
}

#[cfg(test)]
mod tests {
    use super::super::{valid_wayland_display, validate_action};

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/clipboard.rs"
    ));
}

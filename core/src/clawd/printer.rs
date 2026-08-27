use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const PRINT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
const MAX_PRINT_BYTES: u64 = 1024 * 1024 * 1024;
static PRINTER_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("Printer Manager requires Linux CUPS".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Printer Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let action = required_string(&params, "action")?;
        let printer = optional_string(&params, "printer")?;
        let source = optional_string(&params, "source")?;
        let job_id = optional_string(&params, "job_id")?;
        let title = optional_string(&params, "title")?;
        let media = optional_string(&params, "media")?;
        let sides = optional_string(&params, "sides")?;
        let copies = optional_u64(&params, "copies")?.unwrap_or(1);
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            printer.as_deref(),
            source.as_deref(),
            job_id.as_deref(),
            title.as_deref(),
            media.as_deref(),
            sides.as_deref(),
            copies,
            confirm,
        )?;
        let source = source.as_deref().map(resolve_source).transpose()?;
        let requested = requested_caps(&action, source.as_deref());
        let _authorized = authorize_session(authority, &requested)?;
        let user = UserEnvironment::new(uid, gid, home)?;

        match action.as_str() {
            "status" => printer_status(&user).await,
            "jobs" => queue_status(&user, printer.as_deref()).await,
            "capabilities" => printer_capabilities(&user, printer.as_deref().unwrap()).await,
            "print" | "cancel" => {
                let _guard = tokio::time::timeout(
                    LOCK_TIMEOUT,
                    PRINTER_LOCK
                        .get_or_init(|| tokio::sync::Mutex::new(()))
                        .lock(),
                )
                .await
                .map_err(|_| "Printer Manager is busy with another mutation".to_string())?;
                if action == "print" {
                    print_file(
                        &user,
                        printer.as_deref().unwrap(),
                        source.as_deref().unwrap(),
                        copies,
                        title.as_deref(),
                        media.as_deref(),
                        sides.as_deref(),
                    )
                    .await
                } else {
                    cancel_job(&user, job_id.as_deref().unwrap()).await
                }
            }
            _ => unreachable!("validated printer action"),
        }
    }
}

fn requested_caps(action: &str, source: Option<&Path>) -> Vec<Cap> {
    let scope = match action {
        "status" | "capabilities" => {
            return vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("printing"))]
        }
        "jobs" => "observe",
        "print" => "print",
        "cancel" => "control",
        _ => unreachable!("validated printer action"),
    };
    let mut caps = vec![Cap::new(Verb::DEVICE_PRINTER, Scope::name(scope))];
    if let Some(source) = source {
        caps.push(Cap::new(
            Verb::FS_READ,
            Scope::path(source.to_string_lossy().into_owned()),
        ));
    }
    caps
}

/// Final provider check, taken against the decision the broker already
/// made.
///
/// The session, its App identity, the process allowed to act under it,
/// and the capabilities it holds all come from the grant `clawd`
/// issued and the middleware resolved. Nothing here re-reads the
/// process registry or re-derives policy, so the two can no longer
/// disagree; the check still runs, because a privileged mutation
/// should be refused twice.
fn authorize_session(authority: &Decision, requested: &[Cap]) -> Result<Authorized, String> {
    authority.require_app("printer-manager")?;
    authority.require_all(requested)
}

async fn printer_status(user: &UserEnvironment) -> Result<Value, String> {
    let output = run_user_command(
        lpstat_path()?,
        vec!["-p".to_string(), "-d".to_string()],
        user.clone(),
        None,
        QUERY_TIMEOUT,
    )
    .await?;
    let mut printers = Vec::new();
    let mut default = None;
    for line in output.stdout.lines() {
        if let Some(rest) = line.strip_prefix("printer ") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if valid_printer_name(name) {
                printers.push(json!({
                    "name": name,
                    "enabled": !line.contains(" disabled "),
                    "state": line,
                }));
            }
        } else if let Some(value) = line.strip_prefix("system default destination: ") {
            if valid_printer_name(value.trim()) {
                default = Some(value.trim().to_string());
            }
        }
    }
    if !output.status.success()
        && output.stdout.trim().is_empty()
        && !output
            .stderr
            .to_ascii_lowercase()
            .contains("no destinations")
    {
        return Err(format!("lpstat failed: {}", tail(&output.stderr)));
    }
    let count = printers.len();
    Ok(json!({
        "available": true,
        "printers": printers,
        "count": count,
        "default": default,
    }))
}

async fn queue_status(user: &UserEnvironment, printer: Option<&str>) -> Result<Value, String> {
    if let Some(printer) = printer {
        ensure_printer_exists(user, printer).await?;
    }
    let mut args = vec![
        "-W".to_string(),
        "not-completed".to_string(),
        "-o".to_string(),
    ];
    if let Some(printer) = printer {
        args.push(printer.to_string());
    }
    let output = run_user_command(lpstat_path()?, args, user.clone(), None, QUERY_TIMEOUT).await?;
    if !output.status.success() && !output.stderr.to_ascii_lowercase().contains("no entries") {
        return Err(format!("lpstat queue failed: {}", tail(&output.stderr)));
    }
    let jobs = parse_jobs(&output.stdout);
    let count = jobs.len();
    Ok(json!({
        "printer": printer,
        "jobs": jobs,
        "count": count,
    }))
}

fn parse_jobs(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 3 || !valid_job_id(fields[0]) {
                return None;
            }
            Some(json!({
                "id": fields[0],
                "owner": fields[1],
                "size_bytes": fields[2].parse::<u64>().ok(),
                "submitted": fields.get(3..).map(|fields| fields.join(" ")),
            }))
        })
        .collect()
}

async fn printer_capabilities(user: &UserEnvironment, printer: &str) -> Result<Value, String> {
    ensure_printer_exists(user, printer).await?;
    let output = run_user_command(
        lpoptions_path()?,
        vec!["-p".to_string(), printer.to_string(), "-l".to_string()],
        user.clone(),
        None,
        QUERY_TIMEOUT,
    )
    .await?;
    require_success("lpoptions", &output)?;
    let capabilities = output
        .stdout
        .lines()
        .filter_map(|line| {
            let (name, choices) = line.split_once(':')?;
            let (key, description) = name.split_once('/').unwrap_or((name, name));
            Some(json!({
                "key": key,
                "description": description,
                "choices": choices.split_whitespace().map(|choice| json!({
                    "value": choice.trim_start_matches('*'),
                    "default": choice.starts_with('*'),
                })).collect::<Vec<_>>(),
            }))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "printer": printer,
        "capabilities": capabilities,
    }))
}

async fn print_file(
    user: &UserEnvironment,
    printer: &str,
    source: &Path,
    copies: u64,
    title: Option<&str>,
    media: Option<&str>,
    sides: Option<&str>,
) -> Result<Value, String> {
    ensure_printer_exists(user, printer).await?;
    let (mut file, metadata) = open_source(source)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind print source: {error}"))?;
    let title = title
        .map(str::to_string)
        .or_else(|| {
            source
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Claw OS print job".to_string());
    let copies_arg = copies.to_string();
    let mut args = vec![
        "-d".to_string(),
        printer.to_string(),
        "-n".to_string(),
        copies_arg,
        "-t".to_string(),
        title.clone(),
    ];
    if let Some(media) = media {
        args.extend(["-o".to_string(), format!("media={media}")]);
    }
    if let Some(sides) = sides {
        args.extend(["-o".to_string(), format!("sides={sides}")]);
    }
    let output =
        run_user_command(lp_path()?, args, user.clone(), Some(file), PRINT_TIMEOUT).await?;
    require_success("lp", &output)?;
    let job_id = output
        .stdout
        .split_whitespace()
        .find(|value| valid_job_id(value))
        .map(str::to_string);
    Ok(json!({
        "printer": printer,
        "source": source,
        "source_size_bytes": metadata.len(),
        "title": title,
        "copies": copies,
        "job_id": job_id,
        "stdout_tail": tail(&output.stdout),
        "stderr_tail": tail(&output.stderr),
    }))
}

async fn cancel_job(user: &UserEnvironment, job_id: &str) -> Result<Value, String> {
    let jobs = queue_status(user, None).await?;
    let job = jobs["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|job| job["id"].as_str() == Some(job_id))
        .cloned()
        .ok_or_else(|| format!("queued print job not found: {job_id}"))?;
    if user.uid != 0 && job["owner"].as_str() != Some(user.username.as_str()) {
        return Err("refusing to cancel another user's print job".to_string());
    }
    let output = run_user_command(
        cancel_path()?,
        vec![job_id.to_string()],
        user.clone(),
        None,
        QUERY_TIMEOUT,
    )
    .await?;
    require_success("cancel", &output)?;
    Ok(json!({
        "job_id": job_id,
        "canceled": true,
        "before": job,
        "stdout_tail": tail(&output.stdout),
        "stderr_tail": tail(&output.stderr),
    }))
}

fn require_success(program: &str, output: &CommandOutput) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ))
    }
}

async fn ensure_printer_exists(user: &UserEnvironment, printer: &str) -> Result<(), String> {
    if !valid_printer_name(printer) {
        return Err(format!("invalid printer name: {printer:?}"));
    }
    let status = printer_status(user).await?;
    if status["printers"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| item["name"].as_str() == Some(printer))
    {
        Ok(())
    } else {
        Err(format!("printer not found: {printer}"))
    }
}

fn open_source(path: &Path) -> Result<(File, fs::Metadata), String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("open print source {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect print source {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_PRINT_BYTES {
        return Err(format!(
            "print source must be a regular file no larger than {MAX_PRINT_BYTES} bytes"
        ));
    }
    Ok((file, metadata))
}

fn resolve_source(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.len() > 4096
        || !raw.starts_with('/')
        || raw.chars().any(|character| character.is_control())
    {
        return Err("print source must be an absolute path".to_string());
    }
    let metadata = fs::symlink_metadata(raw)
        .map_err(|error| format!("inspect print source {raw:?}: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("print source symlinks are not allowed".to_string());
    }
    fs::canonicalize(raw).map_err(|error| format!("resolve print source: {error}"))
}

#[derive(Clone)]
struct UserEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    username: String,
}

impl UserEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf) -> Result<Self, String> {
        let metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect printer user home {}: {error}", home.display()))?;
        if metadata.uid() != uid {
            return Err(format!(
                "printer user home {} belongs to uid {}, expected {uid}",
                home.display(),
                metadata.uid()
            ));
        }
        Ok(Self {
            uid,
            gid,
            home,
            username: username_for_uid(uid)?,
        })
    }
}

fn username_for_uid(uid: u32) -> Result<String, String> {
    use std::ffi::CStr;
    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || passwd.pw_name.is_null() {
        return Err(format!("passwd entry is unavailable for uid {uid}"));
    }
    unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map(str::to_string)
        .map_err(|_| format!("username is not UTF-8 for uid {uid}"))
}

async fn run_user_command(
    program: &'static str,
    args: Vec<String>,
    user: UserEnvironment,
    stdin_file: Option<File>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || {
        run_user_command_sync(program, args, user, stdin_file, timeout)
    })
    .await
    .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_user_command_sync(
    program: &str,
    args: Vec<String>,
    user: UserEnvironment,
    stdin_file: Option<File>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", &user.home)
        .env("USER", &user.username)
        .env("LOGNAME", &user.username)
        .env("LC_ALL", "C.UTF-8")
        .current_dir(&user.home)
        .stdin(stdin_file.map(Stdio::from).unwrap_or_else(Stdio::null))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let uid = user.uid;
    let gid = user.gid;
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
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
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} stderr is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("wait for timed-out {program}: {error}"))?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for {program}: {error}"));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| format!("{program} stdout reader panicked"))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| format!("{program} stderr reader panicked"))??;
    if timed_out {
        return Err(format!("{program} timed out after {}s", timeout.as_secs()));
    }
    let output = CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    };
    Ok(output)
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    #[allow(dead_code)]
    stdout_truncated: bool,
    #[allow(dead_code)]
    stderr_truncated: bool,
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read CUPS output: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = STREAM_CAP_BYTES.saturating_sub(kept.len());
        let keep = remaining.min(read);
        kept.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((kept, truncated))
}

fn validate_action(
    action: &str,
    printer: Option<&str>,
    source: Option<&str>,
    job_id: Option<&str>,
    title: Option<&str>,
    media: Option<&str>,
    sides: Option<&str>,
    copies: u64,
    confirm: bool,
) -> Result<(), String> {
    if let Some(printer) = printer {
        if !valid_printer_name(printer) {
            return Err("invalid printer name".to_string());
        }
    }
    if let Some(title) = title {
        if title.is_empty()
            || title.len() > 128
            || title.chars().any(|character| character.is_control())
        {
            return Err("invalid print job title".to_string());
        }
    }
    if let Some(media) = media {
        if media.is_empty()
            || media.len() > 128
            || !media
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err("invalid media option".to_string());
        }
    }
    if let Some(sides) = sides {
        if !matches!(
            sides,
            "one-sided" | "two-sided-long-edge" | "two-sided-short-edge"
        ) {
            return Err("invalid sides option".to_string());
        }
    }
    match action {
        "status"
            if printer.is_none()
                && source.is_none()
                && job_id.is_none()
                && title.is_none()
                && media.is_none()
                && sides.is_none()
                && copies == 1
                && !confirm =>
        {
            Ok(())
        }
        "jobs"
            if source.is_none()
                && job_id.is_none()
                && title.is_none()
                && media.is_none()
                && sides.is_none()
                && copies == 1
                && !confirm =>
        {
            Ok(())
        }
        "capabilities"
            if printer.is_some()
                && source.is_none()
                && job_id.is_none()
                && title.is_none()
                && media.is_none()
                && sides.is_none()
                && copies == 1
                && !confirm =>
        {
            Ok(())
        }
        "print"
            if printer.is_some()
                && source.is_some()
                && job_id.is_none()
                && (1..=100).contains(&copies)
                && !confirm =>
        {
            Ok(())
        }
        "cancel"
            if printer.is_none()
                && source.is_none()
                && job_id.is_some_and(valid_job_id)
                && title.is_none()
                && media.is_none()
                && sides.is_none()
                && copies == 1
                && confirm =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for printer action {action:?}")),
    }
}

fn valid_printer_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 127
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_job_id(value: &str) -> bool {
    let Some((printer, id)) = value.rsplit_once('-') else {
        return false;
    };
    valid_printer_name(printer) && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn optional_u64(params: &Value, key: &str) -> Result<Option<u64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("parameter `{key}` must be a non-negative integer")),
        Some(Value::String(value)) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("parameter `{key}` must be an integer")),
        Some(_) => Err(format!("parameter `{key}` must be an integer or null")),
    }
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

fn lpstat_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/lpstat", "/bin/lpstat"], "lpstat")
}
fn lpoptions_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/lpoptions", "/bin/lpoptions"], "lpoptions")
}
fn lp_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/lp", "/bin/lp"], "lp")
}
fn cancel_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/cancel", "/bin/cancel"], "cancel")
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    if value.len() <= MAX {
        return value.trim().to_string();
    }
    let mut start = value.len() - MAX;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].trim().to_string()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/printer.rs"
    ));
}

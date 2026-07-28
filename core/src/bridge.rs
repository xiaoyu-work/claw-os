use std::path::Path;
use std::process::{Command, Stdio};

use std::collections::BTreeMap;

use crate::caps::manifest::{ArgKind, Manifest, Need, Operation, Runtime, ScopeBinding};
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::proc::{deregister_session, register_session, SessionInfo};

pub(crate) fn app_runner_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("CLAW_APP_RUNNER_BIN") {
        return path.into();
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join("claw-app-runner");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    "/usr/local/bin/claw-app-runner".into()
}

fn app_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(app_runner_path());
    command.arg("--").arg(program);
    command
}

fn manifest_app_id(app_dir: &Path) -> Result<String, String> {
    let path = app_dir.join("app.json");
    match std::fs::read_to_string(&path) {
        Ok(body) => crate::apps::AppManifest::from_json(&body)
            .map(|manifest| manifest.id)
            .map_err(|error| format!("parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => app_dir
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("App directory has no UTF-8 id: {}", app_dir.display())),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

pub(crate) struct AppIdentitySession {
    session_id: String,
    backend: AppSessionBackend,
}

#[derive(Clone)]
enum AppSessionBackend {
    Local {
        proc_data_dir: std::path::PathBuf,
    },
    Clawd {
        proc_data_dir: std::path::PathBuf,
    },
}

#[derive(Clone)]
pub(crate) struct AppSessionControl {
    session_id: String,
    backend: AppSessionBackend,
}

pub(crate) struct McpProcSession {
    session_id: String,
    proc_data_dir: std::path::PathBuf,
}

impl McpProcSession {
    pub fn for_current_parent(command: &str) -> Result<Option<Self>, String> {
        #[cfg(unix)]
        if crate::paths::current_owner_uid_override().is_none()
            && unsafe { libc::geteuid() } != 0
        {
            let parent = crate::proc::current_session_info_for_caps()
                .ok_or_else(|| "MCP launch requires a registered parent session".to_string())?;
            let result = clawd_app_session_request(
                "mcp_session.register",
                serde_json::json!({
                    "parent": parent,
                    "command": command,
                }),
            )?;
            let session_id = result
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| "clawd MCP session response omitted session_id".to_string())?;
            let proc_data_dir = result
                .get("proc_data_dir")
                .and_then(serde_json::Value::as_str)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "clawd MCP session response omitted proc_data_dir".to_string())?;
            return Ok(Some(Self {
                session_id,
                proc_data_dir,
            }));
        }
        Ok(None)
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }

    pub fn proc_data_dir(&self) -> &Path {
        &self.proc_data_dir
    }

    pub fn bind_process(&self, pid: u32) -> Result<(), String> {
        clawd_app_session_request(
            "app_session.bind",
            serde_json::json!({
                "session_id": self.session_id,
                "pid": pid,
            }),
        )
        .map(|_| ())
    }
}

impl Drop for McpProcSession {
    fn drop(&mut self) {
        if let Err(error) = clawd_app_session_request(
            "app_session.deregister",
            serde_json::json!({"session_id": self.session_id}),
        ) {
            tracing::warn!(
                session_id = %self.session_id,
                error = %error,
                "failed to deregister MCP child session through clawd"
            );
        }
    }
}

impl AppSessionControl {
    pub fn set_transient_caps(&self, caps: Option<CapSet>) -> Result<(), String> {
        set_app_session_transient_caps(&self.session_id, &self.backend, caps)
    }
}

impl AppIdentitySession {
    /// Register the least-privileged identity for one manifest operation.
    pub fn for_operation(
        app_dir: &Path,
        app_id: &str,
        operation: &str,
        args: &[String],
    ) -> Result<Self, String> {
        if operation == "__schema__" {
            return Err(
                "App schema is generated from app.json and does not execute App code"
                    .to_string(),
            );
        }
        let manifest = load_manifest(app_dir)?;
        let (parent, parent_caps) = Self::parent_identity()?;
        if parent.app_id.is_some() {
            return Err(
                "nested App launches are not supported by the trusted launcher"
                    .to_string(),
            );
        }
        let caps = match (operation, manifest.as_ref()) {
            (_, None) => CapSet::new(),
            (_, Some(manifest)) => {
                let operation = manifest.operations.get(operation).ok_or_else(|| {
                    format!("app `{app_id}` manifest has no operation `{operation}`")
                })?;
                constrained_operation_caps(
                    &parent_caps,
                    false,
                    operation,
                    args,
                )?
            }
        };
        Self::register(
            parent,
            parent_caps,
            app_id,
            &format!("cos app {app_id} {operation}"),
            caps,
        )
    }

    /// Register a GUI identity with the constrained union of all operation needs.
    pub fn for_gui(app_dir: &Path, app_id: &str, exec: &str) -> Result<Self, String> {
        let manifest = load_manifest(app_dir)?;
        let (parent, parent_caps) = Self::parent_identity()?;
        if parent.app_id.is_some() {
            return Err(
                "nested App launches are not supported by the trusted launcher"
                    .to_string(),
            );
        }
        let needs = manifest
            .iter()
            .flat_map(|manifest| manifest.operations.values())
            .flat_map(|operation| operation.needs.iter())
            .collect();
        let caps = constrained_caps(&parent_caps, needs);
        Self::register(
            parent,
            parent_caps,
            app_id,
            &format!("cos app {app_id} {exec}"),
            caps,
        )
    }

    /// Register an MCP identity with the constrained union of all session-tool needs.
    pub fn for_mcp(app_id: &str, manifest: &Manifest) -> Result<Self, String> {
        let (parent, parent_caps) = Self::parent_identity()?;
        if parent.app_id.is_some() {
            return Err(
                "nested App launches are not supported by the trusted launcher"
                    .to_string(),
            );
        }
        let _ = manifest;
        Self::register(
            parent,
            parent_caps,
            app_id,
            &format!("cos app {app_id} session"),
            CapSet::new(),
        )
    }

    fn register(
        parent: SessionInfo,
        parent_caps: CapSet,
        app_id: &str,
        command: &str,
        mut caps: CapSet,
    ) -> Result<Self, String> {
        crate::caps::enforcement::require_current_session_identity(
            &parent.session_id,
            parent.pid,
        )
        .map_err(|err| format!("App parent session identity check failed: {err}"))?;
        let invoke = Cap::new(Verb::AGENT_INVOKE, Scope::name(app_id));
        if !parent_caps.covers(&invoke) {
            return Err(format!("parent session cannot invoke App `{app_id}`"));
        }
        caps.insert(invoke);
        let session_id = format!("app-{}", uuid::Uuid::new_v4().simple());
        let info = SessionInfo {
            session_id: session_id.clone(),
            // Bound to the actual child immediately after spawn. App sessions
            // with pid=0 are denied by caps enforcement during this window.
            pid: 0,
            command: vec![command.to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("app".to_string()),
            parent: Some(parent.session_id),
            workdir: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: parent
                .tier
                .map(|tier| tier.max(crate::caps::Role::Worker.credential_tier())),
            scope: parent.scope,
            priority: parent.priority,
            caps: Some(caps),
            transient_caps: None,
            role: parent.role,
            app_id: Some(app_id.to_string()),
            pending_bind: true,
            start_time_ticks: None,
        };
        let backend = if use_clawd_app_session_backend() {
            register_app_session_with_clawd(&info)?
        } else {
            register_session(info)?;
            AppSessionBackend::Local {
                proc_data_dir: crate::paths::proc_data_dir(),
            }
        };
        Ok(Self {
            session_id,
            backend,
        })
    }

    fn parent_identity() -> Result<(SessionInfo, CapSet), String> {
        let parent = crate::proc::current_session_info_for_caps()
            .ok_or_else(|| "App launch requires a registered parent session".to_string())?;
        let caps = parent
            .caps
            .clone()
            .ok_or_else(|| "App parent session has no capabilities".to_string())?;
        Ok((parent, caps))
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }

    pub fn bind_process(&mut self, pid: u32) -> Result<(), String> {
        match &self.backend {
            AppSessionBackend::Local { .. } => {
                crate::proc::bind_session_process(&self.session_id, pid)
            }
            AppSessionBackend::Clawd { .. } => {
                clawd_app_session_request(
                    "app_session.bind",
                    serde_json::json!({
                        "session_id": self.session_id,
                        "pid": pid,
                    }),
                )
                .map(|_| ())
            }
        }
    }

    pub fn set_transient_caps(&self, caps: Option<CapSet>) -> Result<(), String> {
        set_app_session_transient_caps(&self.session_id, &self.backend, caps)
    }

    pub fn proc_data_dir(&self) -> &Path {
        match &self.backend {
            AppSessionBackend::Local { proc_data_dir }
            | AppSessionBackend::Clawd { proc_data_dir } => proc_data_dir,
        }
    }

    pub fn control(&self) -> AppSessionControl {
        AppSessionControl {
            session_id: self.session_id.clone(),
            backend: self.backend.clone(),
        }
    }
}

fn set_app_session_transient_caps(
    session_id: &str,
    backend: &AppSessionBackend,
    caps: Option<CapSet>,
) -> Result<(), String> {
    match backend {
        AppSessionBackend::Local { .. } => {
            crate::proc::set_app_session_transient_caps(session_id, caps)
        }
        AppSessionBackend::Clawd { .. } => {
            clawd_app_session_request(
                "app_session.set_transient",
                serde_json::json!({
                    "session_id": session_id,
                    "caps": caps,
                }),
            )
            .map(|_| ())
        }
    }
}

fn use_clawd_app_session_backend() -> bool {
    #[cfg(unix)]
    {
        crate::paths::current_owner_uid_override().is_none()
            && unsafe { libc::geteuid() } != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn register_app_session_with_clawd(
    info: &SessionInfo,
) -> Result<AppSessionBackend, String> {
    let result = clawd_app_session_request(
        "app_session.register",
        serde_json::json!({"session": info}),
    )?;
    let proc_data_dir = result
        .get("proc_data_dir")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "clawd App session response omitted proc_data_dir".to_string())?;
    Ok(AppSessionBackend::Clawd { proc_data_dir })
}

fn clawd_app_session_request(
    command: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = crate::clawd::client::request_blocking(
        crate::paths::clawd_socket_path(),
        crate::clawd::protocol::Request {
            id: None,
            command: command.to_string(),
            params,
        },
    )?;
    if response.ok {
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| format!("clawd {command} failed")))
    }
}

fn load_manifest(app_dir: &Path) -> Result<Option<Manifest>, String> {
    let path = app_dir.join("app.json");
    if !path.is_file() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path)
        .map_err(|err| format!("read {}: {err}", path.display()))?;
    Manifest::from_json(&body)
        .map(Some)
        .map_err(|err| format!("parse {}: {err}", path.display()))
}

fn constrained_caps(parent: &CapSet, needs: Vec<&Need>) -> CapSet {
    let mut caps = CapSet::new();
    for need in needs {
        match &need.scope {
            ScopeBinding::Fixed { scope } => {
                let requested = Cap::new(need.verb, scope.clone());
                if parent.covers(&requested) {
                    caps.insert(requested);
                }
            }
            ScopeBinding::Wild => {
                caps.extend(
                    parent
                        .iter()
                        .filter(|cap| cap.verb == need.verb)
                        .cloned(),
                );
            }
            ScopeBinding::FromArg { .. }
            | ScopeBinding::FromArgMap { .. }
            | ScopeBinding::FromArgOrWild { .. } => {}
        }
    }
    caps
}

fn constrained_operation_caps(
    parent: &CapSet,
    parent_is_app: bool,
    operation: &Operation,
    args: &[String],
) -> Result<CapSet, String> {
    let values = parse_operation_args(operation, args);
    let mut caps = CapSet::new();
    for need in &operation.needs {
        let requested = match &need.scope {
            ScopeBinding::Fixed { scope } => Some(Cap::new(need.verb, scope.clone())),
            ScopeBinding::Wild => {
                let inherited = parent
                    .iter()
                    .filter(|cap| cap.verb == need.verb)
                    .cloned()
                    .collect::<Vec<_>>();
                if inherited.is_empty() && !parent_is_app {
                    let requested = Cap::new(need.verb, Scope::Wild);
                    crate::caps::require(requested.verb, requested.scope.clone())
                        .map_err(|denial| denial.summary())?;
                    caps.insert(requested);
                } else {
                    caps.extend(inherited);
                }
                None
            }
            ScopeBinding::FromArg { arg } => operation
                .args
                .iter()
                .find(|decl| decl.name == *arg)
                .and_then(|decl| {
                    values
                        .get(arg)
                        .and_then(|value| scope_for_arg(decl.kind, value))
                })
                .map(|scope| Cap::new(need.verb, scope)),
            ScopeBinding::FromArgMap {
                arg,
                values: mappings,
            } => mappings
                .get(
                    values
                        .get(arg)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )
                .cloned()
                .map(|scope| Cap::new(need.verb, scope)),
            ScopeBinding::FromArgOrWild { arg, wild_when } => {
                if values
                    .get(wild_when)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    Some(Cap::new(need.verb, Scope::Wild))
                } else {
                    operation
                        .args
                        .iter()
                        .find(|decl| decl.name == *arg)
                        .and_then(|decl| {
                            values
                                .get(arg)
                                .and_then(|value| scope_for_arg(decl.kind, value))
                        })
                        .map(|scope| Cap::new(need.verb, scope))
                }
            }
        };
        if let Some(requested) = requested {
            if parent.covers(&requested) {
                caps.insert(requested);
            } else if !parent_is_app {
                crate::caps::require(requested.verb, requested.scope.clone())
                    .map_err(|denial| denial.summary())?;
                caps.insert(requested);
            }
        }
    }
    Ok(caps)
}

fn parse_operation_args(
    operation: &Operation,
    args: &[String],
) -> BTreeMap<String, serde_json::Value> {
    let mut values = BTreeMap::new();
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if let Some(flag) = token.strip_prefix("--") {
            let (name, inline) = flag
                .split_once('=')
                .map(|(name, value)| (name, Some(value)))
                .unwrap_or((flag, None));
            let name = match_arg_name(operation, name);
            if let Some(decl) = name.and_then(|name| {
                operation
                    .args
                    .iter()
                    .find(|decl| decl.name == name)
            }) {
                let raw = inline.map(str::to_string).or_else(|| {
                    if decl.kind != ArgKind::Bool {
                        args.get(index + 1)
                            .filter(|next| !next.starts_with("--"))
                            .cloned()
                    } else {
                        None
                    }
                });
                if inline.is_none() && raw.is_some() {
                    index += 1;
                }
                if let Some(value) = parse_arg_value(decl.kind, raw.as_deref()) {
                    values.insert(decl.name.clone(), value);
                }
            } else if inline.is_none()
                && args
                    .get(index + 1)
                    .is_some_and(|next| !next.starts_with("--"))
            {
                // Unknown flags must not turn their value into a positional
                // capability binding on the next loop iteration.
                index += 1;
            }
        } else {
            positionals.push(token.clone());
        }
        index += 1;
    }

    let mut positional = positionals.into_iter();
    for decl in &operation.args {
        if values.contains_key(&decl.name) {
            continue;
        }
        if decl.kind == ArgKind::Bool {
            values.insert(
                decl.name.clone(),
                decl.default
                    .clone()
                    .unwrap_or(serde_json::Value::Bool(false)),
            );
            continue;
        }
        if let Some(raw) = positional.next() {
            if let Some(value) = parse_arg_value(decl.kind, Some(&raw)) {
                values.insert(decl.name.clone(), value);
            }
        } else if let Some(default) = &decl.default {
            values.insert(decl.name.clone(), default.clone());
        }
    }
    values
}

fn match_arg_name<'a>(operation: &'a Operation, raw: &str) -> Option<&'a str> {
    operation
        .args
        .iter()
        .find(|decl| decl.name == raw || decl.name.replace('_', "-") == raw)
        .map(|decl| decl.name.as_str())
}

fn parse_arg_value(kind: ArgKind, raw: Option<&str>) -> Option<serde_json::Value> {
    match kind {
        ArgKind::Bool => Some(serde_json::Value::Bool(
            raw.map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(true),
        )),
        ArgKind::Number => raw
            .and_then(|value| value.parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number),
        ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => {
            raw.map(|value| serde_json::Value::String(value.to_string()))
        }
    }
}

fn scope_for_arg(kind: ArgKind, value: &serde_json::Value) -> Option<Scope> {
    let value = value.as_str()?;
    match kind {
        ArgKind::Path => Some(Scope::path(value)),
        ArgKind::Host => Some(Scope::host(value)),
        ArgKind::Name => Some(Scope::name(value)),
        ArgKind::Text | ArgKind::Number | ArgKind::Bool => None,
    }
}

fn bind_child_session(
    session: &mut AppIdentitySession,
    child: &mut std::process::Child,
) -> Result<(), String> {
    if let Err(error) = session.bind_process(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(())
}

impl Drop for AppIdentitySession {
    fn drop(&mut self) {
        match &self.backend {
            AppSessionBackend::Local { .. } => deregister_session(&self.session_id),
            AppSessionBackend::Clawd { .. } => {
                if let Err(error) = clawd_app_session_request(
                    "app_session.deregister",
                    serde_json::json!({"session_id": self.session_id}),
                ) {
                    tracing::warn!(
                        session_id = %self.session_id,
                        error = %error,
                        "failed to deregister App session through clawd"
                    );
                }
            }
        }
    }
}

fn reset_app_environment(command: &mut Command) {
    const SAFE_KEYS: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LC_MESSAGES",
        "TZ",
        "TERM",
        "TMPDIR",
        "TEMP",
        "TMP",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
        "PULSE_SERVER",
        "COS_SESSION",
        "COS_TRACE_ID",
        "COS_SPAN_ID",
        "COS_BIN",
        "COS_VERSION",
        "COS_SDK_PYTHON_DIR",
        "COS_SNAPSHOT",
        "COS_PERMS_MODE",
    ];
    let preserved = SAFE_KEYS
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).to_string(), value)))
        .collect::<Vec<_>>();
    command.env_clear();
    command.envs(preserved);
}

#[cfg(unix)]
pub(crate) fn apply_routed_identity(command: &mut Command) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::process::CommandExt;

    let identity = if let Some(uid) = crate::paths::current_owner_uid_override() {
        if uid == 0 {
            return Err("refusing to launch an App as root".to_string());
        }
        let home = crate::paths::current_home_override()
            .ok_or_else(|| format!("missing home directory for App owner uid {uid}"))?;
        let metadata = std::fs::metadata(&home)
            .map_err(|err| format!("inspect App owner home {}: {err}", home.display()))?;
        if metadata.uid() != uid {
            return Err(format!(
                "App owner home {} belongs to uid {}, expected {uid}",
                home.display(),
                metadata.uid()
            ));
        }
        let (gid, username) = account_for_uid(uid)?;
        command.env("USER", &username).env("LOGNAME", username);
        let euid = unsafe { libc::geteuid() } as u32;
        if euid != 0 && euid != uid {
            return Err(format!(
                "cannot launch App for uid {uid} from process uid {euid}"
            ));
        }
        Some((uid, gid, euid))
    } else {
        if crate::paths::is_routed_job() || unsafe { libc::geteuid() } == 0 {
            return Err("refusing to launch an App as root without an owner identity".to_string());
        }
        None
    };
    let expected_parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if let Some((uid, gid, euid)) = identity {
                if euid == 0 {
                    if libc::setgroups(0, std::ptr::null()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setgid(gid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setuid(uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
            }
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != expected_parent {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "App launcher exited before child setup completed",
                    ));
                }
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(unix)]
fn account_for_uid(uid: u32) -> Result<(u32, String), String> {
    use std::ffi::CStr;

    const BUF_SIZE: usize = 16 * 1024;
    let mut buf = vec![0 as libc::c_char; BUF_SIZE];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return Err(format!("passwd lookup failed for App owner uid {uid}"));
    }
    if pwd.pw_name.is_null() {
        return Err(format!("passwd entry for App owner uid {uid} has no name"));
    }
    let username = unsafe { CStr::from_ptr(pwd.pw_name) }
        .to_str()
        .map_err(|_| format!("passwd name for App owner uid {uid} is not UTF-8"))?
        .to_string();
    Ok((pwd.pw_gid as u32, username))
}

#[cfg(not(unix))]
pub(crate) fn apply_routed_identity(_command: &mut Command) -> Result<(), String> {
    if crate::paths::current_owner_uid_override().is_some() {
        return Err("routed App privilege dropping requires Unix".to_string());
    }
    Ok(())
}

/// Build the Python launcher script shared by [`run_python_app`] (one-
/// shot operations) and [`launch_gui`] (long-lived desktop surface).
///
/// The script makes the `claw_os_sdk` + `cos_runtime` packages
/// importable, loads the app's `main.py`, and calls `run(command,
/// args)`. The GUI path passes the manifest's `desktop.exec` value as
/// `command`; an op invocation passes the operation name.
fn python_wrapper(
    main_py: &Path,
    command: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<String, String> {
    Ok(format!(
        r#"
import importlib.util, json, sys, os
os.environ.setdefault("COS_DATA_DIR", {data_dir})
os.environ.setdefault("COS_APPS_DIR", {apps_dir})
# Make the claw_os_sdk + cos_runtime packages importable from every
# app, so Python apps can `from cos_runtime import policy` (capability
# checks) and `from claw_os_sdk import ai` (AI features) without
# bundling either tree into each app. Honour an explicit override;
# otherwise fall back to the common production install path and the
# in-repo dev-checkout paths.
_sdk_override = os.environ.get("COS_SDK_PYTHON_DIR")
_sdk_candidates = []
if _sdk_override:
    _sdk_candidates.append(_sdk_override)
_sdk_candidates.append("/usr/lib/cos/python")
_apps_root = os.environ.get("COS_APPS_DIR") or ""
if _apps_root:
    _sdk_candidates.append(
        os.path.normpath(
            os.path.join(_apps_root, os.pardir, "claw-os-sdk", "python", "src")
        )
    )
    _sdk_candidates.append(
        os.path.normpath(
            os.path.join(_apps_root, os.pardir, "cos-runtime", "python", "src")
        )
    )
_wanted = ("claw_os_sdk", "cos_runtime")
for _cand in _sdk_candidates:
    if not _cand or not os.path.isdir(_cand):
        continue
    if any(os.path.isdir(os.path.join(_cand, _pkg)) for _pkg in _wanted):
        if _cand not in sys.path:
            sys.path.insert(0, _cand)
spec = importlib.util.spec_from_file_location("app", {main_py})
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
result = mod.run({command}, {args})
if result is not None:
    json.dump(result, sys.stdout)
    print()
"#,
        data_dir = serde_json::to_string(data_dir)
            .map_err(|e| format!("failed to serialize data_dir: {e}"))?,
        apps_dir = serde_json::to_string(apps_dir)
            .map_err(|e| format!("failed to serialize apps_dir: {e}"))?,
        main_py = serde_json::to_string(&main_py.to_string_lossy().to_string())
            .map_err(|e| format!("failed to serialize main_py path: {e}"))?,
        command = serde_json::to_string(command)
            .map_err(|e| format!("failed to serialize command: {e}"))?,
        args = serde_json::to_string(args).map_err(|e| format!("failed to serialize args: {e}"))?,
    ))
}

/// Run a Python app's main.py via subprocess.
///
/// Spawns `python3 <app_dir>/main.py` with the command and args passed
/// via a JSON payload on stdin. The app writes JSON to stdout.
///
/// Returns the raw JSON string from stdout, or an error.
pub fn run_python_app(
    app_dir: &Path,
    command: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<Option<String>, String> {
    let main_py = app_dir.join("main.py");
    if !main_py.is_file() {
        return Err(format!("app has no main.py at {}", main_py.display()));
    }

    let wrapper = python_wrapper(&main_py, command, args, data_dir, apps_dir)?;

    let python = if cfg!(windows) { "python" } else { "python3" };

    let app_id = manifest_app_id(app_dir)?;
    let mut app_session =
        AppIdentitySession::for_operation(app_dir, &app_id, command, args)?;

    let mut command = app_command(python);
    reset_app_environment(&mut command);
    command
        .arg("-c")
        .arg(&wrapper)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Agent-native: suppress all interactive prompts
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PIP_NO_INPUT", "1")
        .env("NPM_CONFIG_YES", "true")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("COS_APP_ID", &app_id)
        .env("COS_SESSION", app_session.id())
        .env("COS_DATA_DIR", data_dir)
        .env("COS_PROC_DATA_DIR", app_session.proc_data_dir())
        // Pass config values so Python apps use config.json instead of hardcoded defaults
        .envs(crate::config::as_env_vars());
    if let Some(home) = crate::paths::current_home_override() {
        command.env("HOME", &home).env("COS_HOME", home);
    }
    apply_routed_identity(&mut command)?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn python3: {e}"))?;
    bind_child_session(&mut app_session, &mut child)?;

    // wait_with_output() drains stdout and stderr in background threads
    // BEFORE the child can fill the kernel pipe buffer (Linux default
    // 64KB). The previous pattern of `child.wait()` first and then
    // reading the streams deadlocks for any verb that emits more than
    // 64KB to stdout — e.g. fs.read of a multi-MB file, pkg.list, a
    // wide db.query — because the child blocks on write() while we
    // block on wait().
    let output = child
        .wait_with_output()
        .map_err(|e| format!("python3 wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !status.success() {
        // Try to extract a JSON error from stdout first.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if v.get("error").is_some() {
                return Ok(Some(stdout.trim().to_string()));
            }
        }
        let msg = if stderr.is_empty() {
            format!("exit code {}", status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        };
        return Err(msg);
    }

    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Generic polyglot bridge: read `app_dir/app.json`, pick the runtime
/// based on the `runtime` field (default: python), and invoke the
/// app's entry point.
///
/// Non-Python runtimes get the command + args via env vars instead of
/// the Python wrapper:
///
/// * `COS_COMMAND`     — string (e.g. "ls")
/// * `COS_ARGS_JSON`   — JSON-encoded array of strings
/// * `COS_DATA_DIR`    — same as the python wrapper
/// * `COS_APPS_DIR`    — same
///
/// The app writes one JSON document to stdout. Empty stdout is
/// allowed and reported as `Ok(None)`. On non-zero exit, the
/// function follows the same JSON-error fallback rule as
/// [`run_python_app`]: if stdout parses as `{ "error": ... }` we
/// return that string; otherwise stderr (or the exit code) is
/// returned as an `Err`.
pub fn run_app(
    app_dir: &Path,
    command: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<Option<String>, String> {
    // Load the manifest if present so we can pick a runtime. Apps
    // that ship without app.json default to the Python runtime — this
    // lets ad-hoc `main.py` apps in development still run.
    let manifest_path = app_dir.join("app.json");
    let (runtime, entry) = if manifest_path.is_file() {
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
        let manifest = crate::apps::AppManifest::from_json(&body)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        // Reject app launches whose `ai.tools[]` references a tool
        // the kernel doesn't know. Catches typoed allowlists before
        // the model ever sees a tool definition. The catalog is
        // passed in so the caps crate stays free of an `ai`
        // dependency (would create a cycle).
        let catalog = crate::ai::tools::list_names();
        manifest
            .validate_tools_against_catalog(&catalog)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        let rt = manifest.runtime;
        let entry = manifest
            .entry
            .unwrap_or_else(|| rt.default_entry().to_string());
        (rt, entry)
    } else {
        (Runtime::Python, Runtime::Python.default_entry().to_string())
    };

    if matches!(runtime, Runtime::Python) {
        // Pythonic apps always run through the shared wrapper which
        // loads `main.py`. A custom entry name is currently unsupported
        // for the python runtime; surface a clear error rather than
        // silently ignoring it.
        if entry != "main.py" {
            return Err(format!(
                "python runtime currently requires entry='main.py' (got '{entry}'); \
                 file an issue if you need a per-app entry override"
            ));
        }
        return run_python_app(app_dir, command, args, data_dir, apps_dir);
    }

    let entry_path = app_dir.join(&entry);
    if !entry_path.is_file() {
        return Err(format!("app entry not found: {}", entry_path.display()));
    }

    let args_json =
        serde_json::to_string(args).map_err(|e| format!("failed to serialize args: {e}"))?;

    let mut cmd = match runtime {
        Runtime::Node => {
            let mut c = app_command("node");
            c.arg(&entry_path);
            c
        }
        Runtime::Shell => {
            if cfg!(windows) {
                let mut c = app_command("cmd");
                c.arg("/c").arg(&entry_path);
                c
            } else {
                let mut c = app_command("bash");
                c.arg(&entry_path);
                c
            }
        }
        Runtime::Binary => app_command(&entry_path),
        Runtime::Python => unreachable!("python handled above"),
    };
    let app_id = manifest_app_id(app_dir)?;
    let mut app_session =
        AppIdentitySession::for_operation(app_dir, &app_id, command, args)?;
    reset_app_environment(&mut cmd);

    cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("COS_COMMAND", command)
        .env("COS_ARGS_JSON", &args_json)
        .env("COS_DATA_DIR", data_dir)
        .env("COS_PROC_DATA_DIR", app_session.proc_data_dir())
        .env("COS_APPS_DIR", apps_dir)
        .env("COS_APP_ID", &app_id)
        .env("COS_SESSION", app_session.id())
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PIP_NO_INPUT", "1")
        .env("NPM_CONFIG_YES", "true")
        .envs(crate::config::as_env_vars());
    if let Some(home) = crate::paths::current_home_override() {
        cmd.env("HOME", &home).env("COS_HOME", home);
    }
    apply_routed_identity(&mut cmd)?;
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {runtime:?} app: {e}"))?;
    bind_child_session(&mut app_session, &mut child)?;

    // wait_with_output() avoids the deadlock that occurs when the
    // child writes more than ~64KB to stdout / stderr while we wait
    // — pipe fills, child blocks on write, parent blocks on wait. See
    // run_python_app above for the same fix.
    let output = child
        .wait_with_output()
        .map_err(|e| format!("{runtime:?} app wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !status.success() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            if v.get("error").is_some() {
                return Ok(Some(stdout.trim().to_string()));
            }
        }
        let msg = if stderr.is_empty() {
            format!("exit code {}", status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        };
        return Err(msg);
    }

    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Launch an app's **desktop GUI surface**.
///
/// Unlike [`run_app`] (one-shot, stdout captured as a JSON envelope),
/// this is a long-lived foreground launch: the app entry is spawned
/// with `COS_APP_GUI=1`, given the manifest's `desktop.exec` value
/// (default `--gui`) as its `COS_COMMAND`, inherits the parent's stdio,
/// and runs its own event loop until the window closes.
///
/// Identity (`COS_APP_ID`) is set exactly as for the headless path, so
/// audit / consent / policy enforcement apply unchanged. This is the
/// reason the generated `.desktop` routes through `cos app <id> --gui`
/// instead of exec-ing the app binary directly.
///
/// `exec` is the command the entry receives (the manifest's
/// `desktop.exec`); `files` are the file paths passed by the launcher
/// (`%F`). Returns once the GUI process exits.
pub fn launch_gui(
    app_dir: &Path,
    exec: &str,
    files: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<(), String> {
    let manifest_path = app_dir.join("app.json");
    let (runtime, entry) = if manifest_path.is_file() {
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
        let manifest = crate::apps::AppManifest::from_json(&body)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        let rt = manifest.runtime;
        let entry = manifest
            .entry
            .unwrap_or_else(|| rt.default_entry().to_string());
        (rt, entry)
    } else {
        (Runtime::Python, Runtime::Python.default_entry().to_string())
    };

    let app_id = manifest_app_id(app_dir)?;
    let mut app_session = AppIdentitySession::for_gui(app_dir, &app_id, exec)?;

    let mut cmd = if matches!(runtime, Runtime::Python) {
        let main_py = app_dir.join("main.py");
        if !main_py.is_file() {
            return Err(format!("app has no main.py at {}", main_py.display()));
        }
        let wrapper = python_wrapper(&main_py, exec, files, data_dir, apps_dir)?;
        let python = if cfg!(windows) { "python" } else { "python3" };
        let mut c = app_command(python);
        c.arg("-c").arg(wrapper);
        c
    } else {
        let entry_path = app_dir.join(&entry);
        if !entry_path.is_file() {
            return Err(format!("app entry not found: {}", entry_path.display()));
        }
        match runtime {
            Runtime::Node => {
                let mut c = app_command("node");
                c.arg(&entry_path);
                c
            }
            Runtime::Shell => {
                if cfg!(windows) {
                    let mut c = app_command("cmd");
                    c.arg("/c").arg(&entry_path);
                    c
                } else {
                    let mut c = app_command("bash");
                    c.arg(&entry_path);
                    c
                }
            }
            Runtime::Binary => app_command(&entry_path),
            Runtime::Python => unreachable!("python handled above"),
        }
    };
    reset_app_environment(&mut cmd);

    let args_json =
        serde_json::to_string(files).map_err(|e| format!("failed to serialize files: {e}"))?;
    // A GUI draws on Wayland/X, not stdout. Inherit the parent's stdio
    // so the app's own logging is visible and so it stays attached as a
    // long-lived foreground process until the window is closed.
    cmd
        .stdin(Stdio::null())
        .env("COS_APP_ID", &app_id)
        .env("COS_SESSION", app_session.id())
        .env("COS_APP_GUI", "1")
        .env("COS_COMMAND", exec)
        .env("COS_ARGS_JSON", &args_json)
        .env("COS_DATA_DIR", data_dir)
        .env("COS_APPS_DIR", apps_dir)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("COS_PROC_DATA_DIR", app_session.proc_data_dir())
        .envs(crate::config::as_env_vars());
    if let Some(home) = crate::paths::current_home_override() {
        cmd.env("HOME", &home).env("COS_HOME", home);
    }
    apply_routed_identity(&mut cmd)?;
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch {runtime:?} GUI: {e}"))?;
    bind_child_session(&mut app_session, &mut child)?;
    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for {runtime:?} GUI: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "GUI `{app_id}` exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_entries_are_runtime_aware() {
        assert_eq!(Runtime::Python.default_entry(), "main.py");
        assert_eq!(Runtime::Node.default_entry(), "main.js");
        // Shell + Binary just need to be non-empty.
        assert!(!Runtime::Shell.default_entry().is_empty());
        assert!(!Runtime::Binary.default_entry().is_empty());
    }

    #[test]
    fn run_app_errors_when_app_dir_missing() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-missing");
        let _ = std::fs::remove_dir_all(&tmp);
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        // No app.json + no main.py → python branch surfaces
        // "app has no main.py" via run_python_app.
        assert!(
            err.contains("main.py") || err.contains("app.json"),
            "expected main.py / app.json reference, got: {err}"
        );
    }

    #[test]
    fn run_app_rejects_non_main_py_for_python() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-pyentry");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("app.json"),
            r#"{"id":"x","version":"0","name":"X","runtime":"python","entry":"alt.py"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        assert!(
            err.contains("entry='main.py'"),
            "expected python-entry guard, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_app_errors_on_unknown_runtime() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-unknown");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("app.json"),
            r#"{"id":"x","version":"0","name":"X","runtime":"rust"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        // serde rejects unknown runtime values at parse time.
        assert!(
            err.contains("unknown variant") || err.contains("runtime"),
            "expected runtime parse error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_app_node_entry_missing_surfaces_clear_error() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-node-missing");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("app.json"),
            r#"{"id":"x","version":"0","name":"X","runtime":"node"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        assert!(
            err.contains("app entry not found"),
            "expected entry-missing error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Regression: bridge previously did `child.wait()` BEFORE reading
    /// stdout/stderr. When the child wrote more than the Linux pipe
    /// buffer (~64KB) to stdout, the child blocked on write() while
    /// the parent blocked on wait() — `cos` process hung forever. The
    /// fix routes both run_python_app and run_app through
    /// `wait_with_output`, which drains the streams in background
    /// threads.
    ///
    /// This test asks a tiny Python app to emit a JSON payload well
    /// above 64KB. Before the fix this test would never return; we
    /// add a generous-but-not-infinite outer timeout to make a
    /// regression a quick CI failure instead of a hang.
    #[cfg(unix)]
    #[test]
    fn run_python_app_handles_stdout_larger_than_pipe_buffer() {
        // Skip if python3 isn't on PATH (some minimal CI images).
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let tmp = std::env::temp_dir().join("cos-bridge-test-bigstdout");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // ~256 KB of payload — comfortably over the 64KB pipe buffer.
        std::fs::write(
            tmp.join("main.py"),
            "def run(command, args):\n    return {\"data\": \"x\" * 262144}\n",
        )
        .unwrap();

        // Hard timeout: any deadlock regresses this into a 10s failure
        // rather than a session-killing hang.
        let (tx, rx) = std::sync::mpsc::channel();
        let app_dir = tmp.clone();
        let t = std::thread::spawn(move || {
            let r = run_python_app(&app_dir, "noop", &[], "/tmp", "/tmp");
            let _ = tx.send(r);
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("run_python_app deadlocked on >64KB stdout");
        let _ = t.join();
        let out = result.expect("run_python_app errored").expect("got json");
        assert!(out.len() >= 262_144, "payload truncated, got {} bytes", out.len());
        assert!(out.contains("\"data\""), "json missing data field");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

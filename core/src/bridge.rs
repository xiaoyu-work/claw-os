use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
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
    parent_caps: Option<CapSet>,
}

#[derive(Clone)]
enum AppSessionBackend {
    Local {
        proc_data_dir: std::path::PathBuf,
    },
    /// Session minted by the daemon. `handle` is the opaque, launcher-
    /// bound grant clawd issued at registration; it authorises the pid
    /// bind, transient-cap updates and teardown for this session only,
    /// and is deliberately never exported into the App's environment.
    Clawd {
        proc_data_dir: std::path::PathBuf,
        handle: String,
    },
}

/// What a launcher is asking the authority to start. Mirrors the
/// `kind` discriminator on the `app_session.register` request.
enum LaunchRequest<'a> {
    Operation {
        operation: &'a str,
        args: &'a [String],
    },
    Gui {
        exec: &'a str,
    },
    Mcp,
}

impl LaunchRequest<'_> {
    fn kind(&self) -> &'static str {
        match self {
            LaunchRequest::Operation { .. } => "operation",
            LaunchRequest::Gui { .. } => "gui",
            LaunchRequest::Mcp => "mcp",
        }
    }

    fn command(&self, app_id: &str) -> String {
        match self {
            LaunchRequest::Operation { operation, .. } => format!("cos app {app_id} {operation}"),
            LaunchRequest::Gui { exec } => format!("cos app {app_id} {exec}"),
            LaunchRequest::Mcp => format!("cos app {app_id} session"),
        }
    }
}

/// One serialized App MCP call and the capabilities it needs.
///
/// `tool`/`args` are what the daemon re-derives the call capabilities
/// from; `caps` is the locally resolved set used by the in-process
/// backend, where the resolver already runs inside trusted code.
pub(crate) struct TransientCall<'a> {
    pub tool: &'a str,
    pub args: &'a BTreeMap<String, serde_json::Value>,
    pub caps: CapSet,
}

#[derive(Clone)]
pub(crate) struct AppSessionControl {
    session_id: String,
    backend: AppSessionBackend,
    parent_caps: Option<CapSet>,
}

pub(crate) struct McpProcSession {
    session_id: String,
    proc_data_dir: std::path::PathBuf,
    handle: String,
}

impl McpProcSession {
    pub fn for_current_parent(command: &str) -> Result<Option<Self>, String> {
        #[cfg(unix)]
        if crate::paths::current_owner_uid_override().is_none() && unsafe { libc::geteuid() } != 0 {
            let parent = crate::proc::current_session_info_for_caps()
                .ok_or_else(|| "MCP launch requires a registered parent session".to_string())?;
            if parent.caps.is_none() {
                return Err("MCP parent session has no capabilities".to_string());
            }
            let result = clawd_request(
                "mcp_session.register",
                serde_json::json!({
                    "command": command,
                    "parent_caps": parent.caps,
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
            let handle = result
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| "clawd MCP session response omitted handle".to_string())?;
            return Ok(Some(Self {
                session_id,
                proc_data_dir,
                handle,
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
        clawd_request(
            "app_session.bind",
            serde_json::json!({
                "session_id": self.session_id,
                "handle": self.handle,
                "pid": pid,
            }),
        )
        .map(|_| ())
        .map_err(String::from)
    }
}

impl Drop for McpProcSession {
    fn drop(&mut self) {
        if let Err(error) = clawd_request(
            "app_session.deregister",
            serde_json::json!({
                "session_id": self.session_id,
                "handle": self.handle,
            }),
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
    pub fn set_transient_call(&self, call: Option<TransientCall<'_>>) -> Result<(), String> {
        set_app_session_transient_call(
            &self.session_id,
            &self.backend,
            self.parent_caps.as_ref(),
            call,
        )
    }
}

impl AppIdentitySession {
    pub fn for_native_host(app_id: &str) -> Result<Self, String> {
        let result = clawd_request(
            "app_session.register_native",
            serde_json::json!({"app_id": app_id}),
        )?;
        let session_id = result
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| "clawd native App session response omitted session_id".to_string())?;
        let proc_data_dir = result
            .get("proc_data_dir")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "clawd native App session response omitted proc_data_dir".to_string())?;
        let handle = result
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| "clawd native App session response omitted handle".to_string())?;
        Ok(Self {
            session_id,
            backend: AppSessionBackend::Clawd {
                proc_data_dir,
                handle,
            },
            parent_caps: None,
        })
    }

    /// Register the least-privileged identity for one manifest operation.
    pub fn for_operation(
        app_dir: &Path,
        app_id: &str,
        operation: &str,
        args: &[String],
    ) -> Result<(Self, Vec<String>), String> {
        if operation == "__schema__" {
            return Err(
                "App schema is generated from app.json and does not execute App code".to_string(),
            );
        }

        // Bind the invocation once. The App is launched with these
        // effective arguments and the authority derives the session's
        // capabilities from the same values, so a scope always names the
        // resource the App was actually handed.
        let manifest = load_manifest(app_dir)?;
        let declared = match manifest.as_ref() {
            Some(manifest) => Some(manifest.operations.get(operation).ok_or_else(|| {
                format!("app `{app_id}` manifest has no operation `{operation}`")
            })?),
            None => None,
        };
        let bound = match declared {
            Some(declared) => bind_operation_args(declared, args)?,
            None => BoundOperationArgs {
                values: BTreeMap::new(),
                argv: args.to_vec(),
            },
        };
        let effective_args = bound.argv.clone();
        let session = Self::start(
            app_id,
            LaunchRequest::Operation {
                operation,
                args: &effective_args,
            },
            |parent_caps| match declared {
                Some(declared) => {
                    constrained_operation_caps(parent_caps, false, declared, &bound.values)
                }
                None => Ok(CapSet::new()),
            },
        )?;
        Ok((session, effective_args))
    }

    /// Register a GUI identity with the constrained union of all operation needs.
    pub fn for_gui(app_dir: &Path, app_id: &str, exec: &str) -> Result<Self, String> {
        Self::start(app_id, LaunchRequest::Gui { exec }, |parent_caps| {
            let manifest = load_manifest(app_dir)?;
            let needs = manifest
                .iter()
                .flat_map(|manifest| manifest.operations.values())
                .flat_map(|operation| operation.needs.iter())
                .collect();
            Ok(constrained_caps(parent_caps, needs))
        })
    }

    /// Register an MCP identity. Session tools receive their authority
    /// per call through [`AppSessionControl::set_transient_call`].
    pub fn for_mcp(app_id: &str, manifest: &Manifest) -> Result<Self, String> {
        let _ = manifest;
        Self::start(app_id, LaunchRequest::Mcp, |_| Ok(CapSet::new()))
    }

    /// Shared launch path.
    ///
    /// The parent checks here are a launcher-side sanity gate, not the
    /// authority: when the daemon mints the session it re-derives the
    /// launcher's identity and the App's capabilities itself, and only
    /// ever uses the reported parent capabilities to narrow the result.
    /// `local_caps` is therefore consulted solely for the in-process
    /// backend, which already runs as trusted code.
    fn start<F>(app_id: &str, request: LaunchRequest<'_>, local_caps: F) -> Result<Self, String>
    where
        F: FnOnce(&CapSet) -> Result<CapSet, String>,
    {
        let (parent, parent_caps) = Self::parent_identity()?;
        if parent.app_id.is_some() {
            return Err(
                "nested App launches are not supported by the trusted launcher".to_string(),
            );
        }
        crate::caps::enforcement::require_current_session_identity(&parent.session_id, parent.pid)
            .map_err(|err| format!("App parent session identity check failed: {err}"))?;
        let invoke = Cap::new(Verb::AGENT_INVOKE, Scope::name(app_id));
        if !parent_caps.covers(&invoke) {
            return Err(format!("parent session cannot invoke App `{app_id}`"));
        }

        if use_clawd_app_session_backend() {
            return Self::register_with_clawd(app_id, &request, parent_caps);
        }

        let mut caps = local_caps(&parent_caps)?;
        caps.insert(invoke);
        Self::register_local(&parent, app_id, &request.command(app_id), caps, parent_caps)
    }

    fn register_with_clawd(
        app_id: &str,
        request: &LaunchRequest<'_>,
        parent_caps: CapSet,
    ) -> Result<Self, String> {
        // Only `parent_caps` crosses the wire, and only ever to narrow
        // what the daemon already resolved. The launcher's identity —
        // including the session an approval grant binds to — is derived
        // by `clawd` from this connection, never reported here.
        let mut params = serde_json::json!({
            "app_id": app_id,
            "kind": request.kind(),
            "parent_caps": parent_caps,
        });
        match request {
            LaunchRequest::Operation { operation, args } => {
                params["operation"] = serde_json::Value::String((*operation).to_string());
                params["args"] = serde_json::to_value(args)
                    .map_err(|error| format!("failed to serialize App arguments: {error}"))?;
            }
            LaunchRequest::Gui { exec } => {
                params["operation"] = serde_json::Value::String((*exec).to_string());
            }
            LaunchRequest::Mcp => {}
        }

        // A launch that needs consent is answered with the ids of the
        // requests the daemon filed. This process stays alive and waits,
        // then retries over the same connection identity, so the user
        // never has to rerun anything and no secret has to travel.
        let result = match clawd_request("app_session.register", params.clone()) {
            Ok(result) => result,
            Err(error) => {
                let ids = approval_requests(&error);
                if ids.is_empty() {
                    return Err(error.message);
                }
                wait_for_approvals(&ids)?;
                clawd_request("app_session.register", params).map_err(String::from)?
            }
        };
        let session_id = result
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| "clawd App session response omitted session_id".to_string())?;
        let proc_data_dir = result
            .get("proc_data_dir")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "clawd App session response omitted proc_data_dir".to_string())?;
        let handle = result
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| "clawd App session response omitted handle".to_string())?;
        Ok(Self {
            session_id,
            backend: AppSessionBackend::Clawd {
                proc_data_dir,
                handle,
            },
            parent_caps: Some(parent_caps),
        })
    }

    fn register_local(
        parent: &SessionInfo,
        app_id: &str,
        command: &str,
        caps: CapSet,
        parent_caps: CapSet,
    ) -> Result<Self, String> {
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
            parent: Some(parent.session_id.clone()),
            workdir: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: parent
                .tier
                .map(|tier| tier.max(crate::caps::Role::Worker.credential_tier())),
            scope: parent.scope.clone(),
            priority: parent.priority.clone(),
            caps: Some(caps),
            transient_caps: None,
            role: parent.role.clone(),
            app_id: Some(app_id.to_string()),
            pending_bind: true,
            start_time_ticks: None,
        };
        register_session(info)?;
        Ok(Self {
            session_id,
            backend: AppSessionBackend::Local {
                proc_data_dir: crate::paths::proc_data_dir(),
            },
            parent_caps: Some(parent_caps),
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
            AppSessionBackend::Clawd { handle, .. } => clawd_request(
                "app_session.bind",
                serde_json::json!({
                    "session_id": self.session_id,
                    "handle": handle,
                    "pid": pid,
                }),
            )
            .map(|_| ())
            .map_err(String::from),
        }
    }

    pub fn set_transient_call(&self, call: Option<TransientCall<'_>>) -> Result<(), String> {
        set_app_session_transient_call(
            &self.session_id,
            &self.backend,
            self.parent_caps.as_ref(),
            call,
        )
    }

    pub fn proc_data_dir(&self) -> &Path {
        match &self.backend {
            AppSessionBackend::Local { proc_data_dir }
            | AppSessionBackend::Clawd { proc_data_dir, .. } => proc_data_dir,
        }
    }

    pub fn control(&self) -> AppSessionControl {
        AppSessionControl {
            session_id: self.session_id.clone(),
            backend: self.backend.clone(),
            parent_caps: self.parent_caps.clone(),
        }
    }
}

pub fn run_native_app_host(
    app_id: &str,
    app_dir: &Path,
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
) -> Result<(), String> {
    if app_id != "mail-ai" {
        return Err("native App host is restricted to mail-ai".to_string());
    }
    let app_dir = app_dir
        .canonicalize()
        .map_err(|error| format!("canonicalize native App directory: {error}"))?;
    if app_dir != Path::new("/usr/lib/cos/mail-ai") {
        return Err(format!(
            "native mail-ai host must run from /usr/lib/cos/mail-ai, got {}",
            app_dir.display()
        ));
    }
    let program_path = Path::new(program)
        .canonicalize()
        .map_err(|error| format!("canonicalize native App program: {error}"))?;
    let expected_python = Path::new("/usr/bin/python3")
        .canonicalize()
        .map_err(|error| format!("canonicalize /usr/bin/python3: {error}"))?;
    let isolated = args.first().and_then(|value| value.to_str()) == Some("-I");
    let host_arg = args.get(1).map(std::path::PathBuf::from);
    let expected_host = app_dir.join("native_host.py");
    if program_path != expected_python
        || !isolated
        || host_arg.as_deref() != Some(expected_host.as_path())
    {
        return Err(
            "native mail-ai host command does not match the installed launcher".to_string(),
        );
    }
    let runner = Path::new("/usr/local/bin/claw-app-runner")
        .canonicalize()
        .map_err(|error| format!("canonicalize native App runner: {error}"))?;
    validate_root_owned_executable(&runner)?;
    validate_root_owned_executable(&program_path)?;
    validate_root_owned_executable(&expected_host)?;
    let manifest_id = manifest_app_id(&app_dir)?;
    if manifest_id != app_id {
        return Err(format!(
            "native host App id `{app_id}` does not match manifest `{manifest_id}`"
        ));
    }
    let mut app_session = AppIdentitySession::for_native_host(app_id)?;
    let mut command = Command::new(&runner);
    command.arg("--").arg(&program_path);
    reset_app_environment(&mut command, false);
    command
        .args(args)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("COS_BIN", "/usr/local/bin/cos")
        .env("COS_APPS_DIR", "/usr/lib/cos/apps")
        .env("COS_SDK_PYTHON_DIR", "/usr/lib/cos/python")
        .env("PYTHONNOUSERSITE", "1")
        .env_remove("CLAW_APP_RUNNER_BIN")
        .env_remove("CLAW_PYTHON_LIB")
        .env("COS_APP_ID", app_id)
        .env("COS_SESSION", app_session.id())
        .env("COS_PROC_DATA_DIR", app_session.proc_data_dir())
        .env("COS_DATA_DIR", crate::paths::user_data_dir());
    apply_routed_identity(&mut command)?;
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn native App host: {error}"))?;
    bind_child_session(&mut app_session, &mut child)?;
    let status = child
        .wait()
        .map_err(|error| format!("wait for native App host: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "native App host exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(unix)]
fn validate_root_owned_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "native App executable must be root-owned and not group/world-writable: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_root_owned_executable(_path: &Path) -> Result<(), String> {
    Err("native App host requires Unix ownership checks".to_string())
}

fn set_app_session_transient_call(
    session_id: &str,
    backend: &AppSessionBackend,
    parent_caps: Option<&CapSet>,
    call: Option<TransientCall<'_>>,
) -> Result<(), String> {
    match backend {
        AppSessionBackend::Local { .. } => {
            crate::proc::set_app_session_transient_caps(session_id, call.map(|call| call.caps))
        }
        AppSessionBackend::Clawd { handle, .. } => {
            let call = match call {
                Some(call) => serde_json::json!({
                    "tool": call.tool,
                    "args": call.args,
                }),
                None => serde_json::Value::Null,
            };
            let mut params = serde_json::json!({
                "session_id": session_id,
                "handle": handle,
                "call": call,
            });
            if let Some(parent_caps) = parent_caps {
                params["parent_caps"] = serde_json::to_value(parent_caps).map_err(|error| {
                    format!("failed to serialize parent capabilities: {error}")
                })?;
            }
            clawd_request("app_session.set_transient", params)
                .map(|_| ())
                .map_err(String::from)
        }
    }
}

fn use_clawd_app_session_backend() -> bool {
    #[cfg(test)]
    if std::env::var_os("COS_TEST_LOCAL_APP_SESSIONS").is_some() {
        return false;
    }
    #[cfg(unix)]
    {
        crate::paths::current_owner_uid_override().is_none() && unsafe { libc::geteuid() } != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// A failed broker call plus whatever structured payload the daemon
/// attached for this caller only.
struct ClawdCallError {
    message: String,
    data: Option<serde_json::Value>,
}

impl From<ClawdCallError> for String {
    fn from(error: ClawdCallError) -> String {
        error.message
    }
}

impl std::fmt::Display for ClawdCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn clawd_request(
    command: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ClawdCallError> {
    let response = crate::clawd::client::request_blocking(
        crate::paths::clawd_socket_path(),
        crate::clawd::protocol::Request {
            id: None,
            command: command.to_string(),
            params,
        },
    )
    .map_err(|message| ClawdCallError {
        message,
        data: None,
    })?;
    if response.ok {
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    } else {
        let (message, data) = match response.error {
            Some(error) => (error.message, error.data),
            None => (format!("clawd {command} failed"), None),
        };
        Err(ClawdCallError { message, data })
    }
}

/// Longest a launcher will hold its place while the user decides.
const APPROVAL_WAIT: Duration = Duration::from_secs(120);
const APPROVAL_POLL: Duration = Duration::from_millis(500);

/// Abandon an in-flight approval wait.
///
/// The launcher blocks in its own process so its authenticated identity
/// stays valid for the retry; a host that no longer wants the launch
/// (an interrupted agent turn, a test) flips this and the wait ends
/// with a terminal error. Ctrl-C at a terminal ends the process itself.
static APPROVAL_WAIT_CANCELLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn cancel_pending_approval_wait() {
    APPROVAL_WAIT_CANCELLED.store(true, Ordering::SeqCst);
}

/// Approval request ids a denied launch is waiting on, if the daemon
/// reported any. Ids are not authority — they only say which decisions
/// this launcher needs.
fn approval_requests(error: &ClawdCallError) -> Vec<String> {
    error
        .data
        .as_ref()
        .filter(|data| data.get("status").and_then(serde_json::Value::as_str)
            == Some("approval_required"))
        .and_then(|data| data.get("approval_requests"))
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Block this process until every listed request is decided.
///
/// Waiting in the launcher is what keeps the retry authentic: `clawd`
/// re-derives the same uid/pid/start identity on the follow-up call, so
/// no token, environment variable or session string has to travel. The
/// wait is bounded, ends immediately on a rejection, and reports a
/// terminal error for anything that is not a clean approval.
fn wait_for_approvals(ids: &[String]) -> Result<(), String> {
    let deadline = Instant::now() + APPROVAL_WAIT;
    loop {
        if APPROVAL_WAIT_CANCELLED.swap(false, Ordering::SeqCst) {
            return Err("waiting for App launch approval was cancelled".to_string());
        }
        let result = clawd_request(
            "permission.status",
            serde_json::json!({"ids": ids}),
        )
        .map_err(String::from)?;
        let statuses = result
            .get("statuses")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut pending = false;
        for entry in &statuses {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match entry.get("status").and_then(serde_json::Value::as_str) {
                Some("approved") => {}
                Some("pending") => pending = true,
                Some("denied") => {
                    return Err(format!("App launch approval {id} was denied"));
                }
                other => {
                    return Err(format!(
                        "App launch approval {id} is no longer available ({})",
                        other.unwrap_or("unknown")
                    ));
                }
            }
        }
        if !pending {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for App launch approval",
                APPROVAL_WAIT.as_secs()
            ));
        }
        std::thread::sleep(APPROVAL_POLL);
    }
}

fn load_manifest(app_dir: &Path) -> Result<Option<Manifest>, String> {
    let path = app_dir.join("app.json");
    if !path.is_file() {
        return Ok(None);
    }
    let body =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
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
                caps.extend(parent.iter().filter(|cap| cap.verb == need.verb).cloned());
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
    values: &BTreeMap<String, serde_json::Value>,
) -> Result<CapSet, String> {
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
                        .map_err(|denial| denial.to_string())?;
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
                    .map_err(|denial| denial.to_string())?;
                caps.insert(requested);
            }
        }
    }
    Ok(caps)
}

#[derive(Debug)]
struct BoundOperationArgs {
    values: BTreeMap<String, serde_json::Value>,
    argv: Vec<String>,
}

fn bind_operation_args(
    operation: &Operation,
    args: &[String],
) -> Result<BoundOperationArgs, String> {
    let mut values = parse_supplied_operation_args(operation, args);
    for declaration in &operation.args {
        if declaration.required && !values.contains_key(&declaration.name) {
            return Err(format!(
                "required operation arg `{}` was not supplied",
                declaration.name
            ));
        }
    }
    let defaulted = operation
        .apply_arg_defaults(&mut values)
        .map_err(|error| format!("resolve operation defaults: {error}"))?;
    normalize_path_args(operation, &mut values)?;

    let mut argv = args.to_vec();
    for name in defaulted {
        let value = values
            .get(&name)
            .ok_or_else(|| format!("resolved default for `{name}` is missing"))?;
        // Boolean declarations are never positional, so appending a
        // resolved default as a bare token would both hand the App an
        // argument it never asked for and shift the next positional the
        // authority re-binds from this argv. The value stays in
        // `values`, and the authority fills it from the same manifest
        // default, so both sides still agree.
        if operation
            .args
            .iter()
            .any(|declaration| declaration.name == name && declaration.kind == ArgKind::Bool)
        {
            continue;
        }
        argv.push(arg_value_to_string(value)?);
    }
    for declaration in &operation.args {
        if declaration.kind == ArgKind::Bool && !values.contains_key(&declaration.name) {
            values.insert(declaration.name.clone(), serde_json::Value::Bool(false));
        }
    }
    Ok(BoundOperationArgs { values, argv })
}

fn parse_supplied_operation_args(
    operation: &Operation,
    args: &[String],
) -> BTreeMap<String, serde_json::Value> {
    crate::caps::args::bind_supplied_cli_args(&operation.args, args)
}

fn normalize_path_args(
    operation: &Operation,
    values: &mut BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    crate::caps::args::resolve_path_args(&operation.args, values, &launcher_path_context()?)
}

/// Where this launcher resolves relative and `~` path arguments.
fn launcher_path_context() -> Result<crate::caps::args::PathContext, String> {
    Ok(crate::caps::args::PathContext {
        home: effective_app_home(),
        cwd: Some(
            std::env::current_dir()
                .map_err(|error| format!("resolve current directory for path arg: {error}"))?,
        ),
    })
}

fn effective_app_home() -> std::path::PathBuf {
    crate::paths::current_home_override()
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("/root"))
}

fn arg_value_to_string(value: &serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::String(value) => Ok(value.clone()),
        serde_json::Value::Number(value) => Ok(value.to_string()),
        serde_json::Value::Bool(value) => Ok(value.to_string()),
        _ => Err("manifest defaults must be strings, numbers, or booleans".to_string()),
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
            AppSessionBackend::Clawd { handle, .. } => {
                if let Err(error) = clawd_request(
                    "app_session.deregister",
                    serde_json::json!({
                        "session_id": self.session_id,
                        "handle": handle,
                    }),
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

const SAFE_APP_ENV_KEYS: &[&str] = &[
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

const PANEL_APPLET_ENV_KEYS: &[&str] = &[
    "WAYLAND_SOCKET",
    "X_PRIVILEGED_WAYLAND_SOCKET",
    "COSMIC_PANEL_NAME",
    "COSMIC_PANEL_OUTPUT",
    "COSMIC_PANEL_SPACING",
    "COSMIC_PANEL_ANCHOR",
    "COSMIC_PANEL_BACKGROUND",
    "COSMIC_PANEL_PADDING_OVERLAP",
    "COSMIC_PANEL_SIZE",
];

fn preserved_app_environment<F>(
    panel_applet: bool,
    mut value_for: F,
) -> Vec<(String, String)>
where
    F: FnMut(&str) -> Option<String>,
{
    let panel_keys: &[&str] = if panel_applet {
        PANEL_APPLET_ENV_KEYS
    } else {
        &[]
    };
    SAFE_APP_ENV_KEYS
        .iter()
        .chain(panel_keys)
        .filter_map(|key| {
            value_for(key).map(|value| ((*key).to_string(), value))
        })
        .collect()
}

fn reset_app_environment(command: &mut Command, panel_applet: bool) {
    let preserved =
        preserved_app_environment(panel_applet, |key| std::env::var(key).ok());
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

    let python = if cfg!(windows) { "python" } else { "python3" };

    let app_id = manifest_app_id(app_dir)?;
    let (mut app_session, effective_args) =
        AppIdentitySession::for_operation(app_dir, &app_id, command, args)?;
    let wrapper = python_wrapper(&main_py, command, &effective_args, data_dir, apps_dir)?;

    let mut command = app_command(python);
    reset_app_environment(&mut command, false);
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
    let (mut app_session, effective_args) =
        AppIdentitySession::for_operation(app_dir, &app_id, command, args)?;
    let args_json = serde_json::to_string(&effective_args)
        .map_err(|e| format!("failed to serialize args: {e}"))?;
    reset_app_environment(&mut cmd, false);

    cmd.stdin(Stdio::null())
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
    let (runtime, entry, panel_applet) = if manifest_path.is_file() {
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
        let manifest = crate::apps::AppManifest::from_json(&body)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        let rt = manifest.runtime;
        let panel_applet = manifest
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.panel_applet);
        let entry = manifest
            .entry
            .unwrap_or_else(|| rt.default_entry().to_string());
        (rt, entry, panel_applet)
    } else {
        (
            Runtime::Python,
            Runtime::Python.default_entry().to_string(),
            false,
        )
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
    reset_app_environment(&mut cmd, panel_applet);

    let args_json =
        serde_json::to_string(files).map_err(|e| format!("failed to serialize files: {e}"))?;
    // A GUI draws on Wayland/X, not stdout. Inherit the parent's stdio
    // so the app's own logging is visible and so it stays attached as a
    // long-lived foreground process until the window is closed.
    cmd.stdin(Stdio::null())
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge.rs"
    ));
}

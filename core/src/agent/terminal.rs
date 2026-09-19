//! Owns the terminal frontend process; Agent execution stays behind clawd.

use std::path::{Path, PathBuf};

use serde_json::Value;

const FRONTEND_CONFIG: &[&str] = &[
    "cli_auth_credentials_store=\"ephemeral\"",
    "check_for_update_on_startup=false",
    "analytics.enabled=false",
    "feedback.enabled=false",
    "otel.exporter=\"none\"",
    "otel.trace_exporter=\"none\"",
    "otel.metrics_exporter=\"none\"",
    "otel.log_user_prompt=false",
    "web_search=\"disabled\"",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Auto,
    Plain,
    Tui,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ChatOptions {
    mode: Mode,
    pub session_id: Option<String>,
    pub no_stream: bool,
    pub no_memory: bool,
    pub show_tools: bool,
    pub max_turns: Option<u32>,
    frontend_args: Vec<String>,
}

impl ChatOptions {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--app" => {
                    return Err(
                        "`cos agent chat` is the kernel Agent's UI and does not accept --app. \
                         For App-gated calls use `cos ai chat --app <id>`."
                            .into(),
                    );
                }
                "--plain" => options.set_mode(Mode::Plain)?,
                "--tui" => options.set_mode(Mode::Tui)?,
                "--session" => {
                    options.session_id = Some(
                        args.next()
                            .filter(|value| !value.trim().is_empty())
                            .ok_or("--session needs a non-empty id")?
                            .clone(),
                    );
                }
                "--no-stream" => options.no_stream = true,
                "--no-memory" => options.no_memory = true,
                "--show-tools" => options.show_tools = true,
                "--max-turns" => {
                    options.max_turns = Some(
                        args.next()
                            .ok_or("--max-turns needs <n>")?
                            .parse()
                            .map_err(|error| format!("--max-turns: {error}"))?,
                    );
                }
                "--" => {
                    options.frontend_args = args.cloned().collect();
                    validate_frontend_args(&options.frontend_args)?;
                    break;
                }
                other => return Err(format!("unknown flag for `chat`: {other}")),
            }
        }
        if options.mode == Mode::Tui && (options.no_stream || options.show_tools) {
            return Err("--no-stream and --show-tools belong to --plain chat".into());
        }
        if !options.frontend_args.is_empty()
            && (options.mode == Mode::Plain || options.no_stream || options.show_tools)
        {
            return Err("frontend arguments after -- cannot be used with --plain chat".into());
        }
        Ok(options)
    }

    fn set_mode(&mut self, mode: Mode) -> Result<(), String> {
        if self.mode != Mode::Auto && self.mode != mode {
            return Err("--plain and --tui cannot be combined".into());
        }
        self.mode = mode;
        Ok(())
    }

    pub fn use_tui(&self, interactive: bool, dumb_terminal: bool) -> Result<bool, String> {
        if self.mode == Mode::Plain || self.no_stream || self.show_tools {
            return Ok(false);
        }
        if !interactive || dumb_terminal {
            if self.mode == Mode::Tui || !self.frontend_args.is_empty() {
                return Err(
                    "the Agent TUI needs an interactive terminal; use --plain for pipes or TERM=dumb"
                        .into(),
                );
            }
            return Ok(false);
        }
        Ok(true)
    }
}

fn validate_frontend_args(args: &[String]) -> Result<(), String> {
    for (index, arg) in args.iter().enumerate() {
        let flag = arg.split('=').next().unwrap_or(arg);
        if matches!(
            flag,
            "--remote"
                | "--remote-transport"
                | "--remote-auth-token-env"
                | "--cd"
                | "-C"
                | "--sandbox"
                | "-s"
                | "--ask-for-approval"
                | "-a"
                | "--full-auto"
                | "--dangerously-bypass-approvals-and-sandbox"
                | "--"
        ) {
            return Err(format!(
                "{flag} is owned by Claw OS; frontend arguments cannot replace its backend or policy"
            ));
        }
        let setting = if matches!(arg.as_str(), "-c" | "--config") {
            Some(
                args.get(index + 1)
                    .ok_or("frontend --config needs key=value")?
                    .as_str(),
            )
        } else {
            arg.strip_prefix("--config=")
                .or_else(|| arg.strip_prefix("-c").filter(|value| !value.is_empty()))
        };
        if let Some(setting) = setting {
            let (key, _) = setting
                .split_once('=')
                .ok_or("frontend --config needs key=value")?;
            let key = key.trim();
            if FRONTEND_CONFIG.iter().any(|managed| {
                let managed = managed.split('=').next().expect("fixed config key");
                key == managed
                    || managed.starts_with(&format!("{key}."))
                    || key.starts_with(&format!("{managed}."))
            }) {
                return Err(format!(
                    "frontend configuration {key} is managed by Claw OS"
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn interactive_terminal() -> bool {
    use std::io::IsTerminal;

    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

pub(super) fn dumb_terminal() -> bool {
    std::env::var_os("TERM").is_some_and(|value| value == "dumb")
}

fn executable() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("COS_AGENT_TUI_BIN") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("COS_AGENT_TUI_BIN must be an absolute executable path".into());
        }
        return require_executable(&path);
    }
    let installed = Path::new("/usr/lib/cos/tui/bin/codex-tui");
    if installed.try_exists().map_err(|error| error.to_string())? {
        return require_executable(installed);
    }
    let development = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("build")
        .join("agent-tui")
        .join("bin")
        .join("codex-tui");
    if development
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return require_executable(&development);
    }
    Err(
        "the Agent TUI binary is not installed; build it using terminal/README.md \
         or reinstall claw-os-agent. Use `cos agent chat --plain` for the line interface."
            .into(),
    )
}

fn require_executable(path: &Path) -> Result<PathBuf, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot inspect TUI executable {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("TUI executable is not a file: {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("TUI file is not executable: {}", path.display()));
        }
    }
    std::fs::canonicalize(path)
        .map_err(|error| format!("cannot resolve TUI executable {}: {error}", path.display()))
}

#[cfg(unix)]
fn frontend_home() -> Result<PathBuf, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let home = crate::paths::user_data_dir().join("terminal");
    std::fs::create_dir_all(&home)
        .map_err(|error| format!("cannot create terminal state directory: {error}"))?;
    let metadata = std::fs::symlink_metadata(&home)
        .map_err(|error| format!("cannot inspect terminal state directory: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("terminal state must be a directory owned by the current user".into());
    }
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect terminal state directory: {error}"))?;
    Ok(home)
}

#[cfg(unix)]
pub(super) fn run(options: ChatOptions) -> Result<Value, String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.into());
    }
    crate::update::runtime::enforce_startup(crate::update::runtime::Scope::CriticalComponents)
        .map_err(|refusal| refusal.message)?;
    let executable = executable()?;
    let home = frontend_home()?;
    let config = crate::config::current_snapshot();
    super::setup::is_ready(&config.agent)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("terminal runtime: {error}"))?;
    runtime.block_on(run_async(executable, home, config, options))
}

#[cfg(unix)]
async fn run_async(
    executable: PathBuf,
    home: PathBuf,
    config: std::sync::Arc<crate::config::CosConfig>,
    options: ChatOptions,
) -> Result<Value, String> {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    let owner_home = crate::paths::verified_home_for_uid(unsafe { libc::geteuid() })?;
    let socket_dir = tempfile::Builder::new()
        .prefix("cos-agent-tui-")
        .tempdir()
        .map_err(|error| format!("cannot create private TUI socket directory: {error}"))?;
    let socket_path = socket_dir.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path)
        .map_err(|error| format!("cannot bind private TUI socket: {error}"))?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot protect private TUI socket: {error}"))?;

    let resume_id = match options.session_id.as_deref() {
        Some(id) => Some(super::tui_backend::initial_thread_id(id).await?),
        None => None,
    };
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let backend_options = super::tui_backend::Options {
        use_memory: !options.no_memory,
        max_turns: options.max_turns,
        session_id: options.session_id,
    };
    let mut server = tokio::spawn(super::tui_backend::serve(
        listener,
        config,
        backend_options,
        receiver,
    ));
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("--remote")
        .arg(format!("unix://{}", socket_path.display()))
        .arg("--cd")
        .arg(&owner_home)
        .arg("--sandbox")
        .arg("danger-full-access")
        .arg("--ask-for-approval")
        .arg("on-request")
        .args(&options.frontend_args)
        .current_dir(&owner_home)
        .env("CODEX_HOME", home)
        .env("COS_TUI_FRONTEND", "1")
        .env("HOME", owner_home)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for setting in FRONTEND_CONFIG {
        command.arg("--config").arg(setting);
    }
    for (name, _) in std::env::vars_os() {
        if private_backend_environment(&name) {
            command.env_remove(name);
        }
    }
    if let Some(id) = resume_id {
        command.arg("resume").arg(id);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = shutdown.send(true);
            let backend = server.await;
            if let Err(backend_error) = flatten_backend_result(backend) {
                tracing::error!(%backend_error, "TUI backend shutdown failed after frontend spawn");
            }
            return Err(format!("cannot start Agent TUI: {error}"));
        }
    };

    let outcome = tokio::select! {
        status = child.wait() => {
            status
                .map_err(|error| format!("cannot wait for Agent TUI: {error}"))
                .and_then(|status| {
                    if status.success() {
                        Ok(Value::Null)
                    } else {
                        Err(format!("Agent TUI exited with {status}"))
                    }
                })
        }
        backend = &mut server => {
            if let Err(error) = child.kill().await {
                tracing::error!(%error, "could not stop frontend after TUI backend ended");
            }
            return match flatten_backend_result(backend) {
                Ok(()) => Err("Agent TUI backend ended before the frontend".into()),
                Err(error) => Err(error),
            };
        }
    };
    let _ = shutdown.send(true);
    flatten_backend_result(server.await)?;
    outcome
}

fn private_backend_environment(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with("OPENAI_")
        || name.starts_with("CODEX_EXEC_SERVER_")
        || name.starts_with("OTEL_")
        || matches!(name.as_ref(), "CODEX_API_KEY" | "CODEX_ACCESS_TOKEN")
}

#[cfg(unix)]
fn flatten_backend_result(
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), String> {
    result.map_err(|error| format!("Agent TUI backend task failed: {error}"))?
}

#[cfg(not(unix))]
pub(super) fn run(_options: ChatOptions) -> Result<Value, String> {
    Err("the Agent TUI requires Linux or WSL".into())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/terminal.rs"
    ));
}

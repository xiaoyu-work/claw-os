use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(180);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const MAX_LOG_LINES: u64 = 1000;
static CONTAINER_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Container Manager requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Container Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let runtime = optional_string(&params, "runtime")?;
        let target = optional_string(&params, "target")?;
        let namespace = optional_string(&params, "namespace")?;
        let signal = optional_string(&params, "signal")?;
        let lines = optional_u64(&params, "lines")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            runtime.as_deref(),
            target.as_deref(),
            namespace.as_deref(),
            signal.as_deref(),
            lines,
            confirm,
        )?;
        let runtime = runtime
            .as_deref()
            .map(ContainerRuntime::parse)
            .transpose()?;
        if let Some(target) = target.as_deref() {
            validate_identifier("container", target)?;
        }
        if let Some(namespace) = namespace.as_deref() {
            validate_identifier("containerd namespace", namespace)?;
        }
        let scope = if is_mutating(&action) {
            "control"
        } else {
            "observe"
        };
        let requested = Cap::new(Verb::SYS_CONTAINER, Scope::name(scope));
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, requested)
        })
        .await?;
        let user = UserEnvironment::new(uid, gid, home)?;

        if !is_mutating(&action) {
            return read_action(
                &action,
                runtime,
                target.as_deref(),
                namespace.as_deref(),
                lines,
                &user,
            )
            .await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            CONTAINER_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Container Manager is busy with another mutation".to_string())?;
        mutate(
            &action,
            runtime.expect("validated container runtime"),
            target.as_deref().expect("validated container target"),
            namespace.as_deref(),
            signal.as_deref(),
            confirm,
            &user,
        )
        .await
    }
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: Cap) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("container-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("container-manager") {
        return Err("container control is restricted to the container-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("container-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "container-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("container-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("container request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&requested) {
        return Err(format!(
            "container-manager session lacks {}:{}",
            requested.verb.as_str(),
            requested.scope
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerRuntime {
    Docker,
    Podman,
    PodmanRoot,
    Containerd,
}

impl ContainerRuntime {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "docker" => Ok(Self::Docker),
            "podman" => Ok(Self::Podman),
            "podman-root" => Ok(Self::PodmanRoot),
            "containerd" => Ok(Self::Containerd),
            _ => Err(format!(
                "unknown container runtime {value:?}; expected docker, podman, podman-root, or containerd"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
            Self::PodmanRoot => "podman-root",
            Self::Containerd => "containerd",
        }
    }

    fn identity(self, user: &UserEnvironment) -> Result<Option<UserEnvironment>, String> {
        if self == Self::Podman {
            user.validate_runtime()?;
            Ok(Some(user.clone()))
        } else {
            Ok(None)
        }
    }
}

async fn read_action(
    action: &str,
    runtime: Option<ContainerRuntime>,
    target: Option<&str>,
    namespace: Option<&str>,
    lines: Option<u64>,
    user: &UserEnvironment,
) -> Result<Value, String> {
    match action {
        "status" => status(user).await,
        "list" => list_containers(runtime.unwrap(), namespace, user).await,
        "inspect" => inspect_container(runtime.unwrap(), target.unwrap(), namespace, user).await,
        "logs" => {
            container_logs(
                runtime.unwrap(),
                target.unwrap(),
                namespace,
                lines.unwrap_or(100),
                user,
            )
            .await
        }
        "processes" => {
            container_processes(runtime.unwrap(), target.unwrap(), namespace, user).await
        }
        "stats" => container_stats(runtime.unwrap(), target.unwrap(), namespace, user).await,
        "namespaces" => namespace_report(runtime.unwrap(), target.unwrap(), namespace, user).await,
        _ => unreachable!("validated container read action"),
    }
}

async fn status(user: &UserEnvironment) -> Result<Value, String> {
    let docker = list_containers(ContainerRuntime::Docker, None, user);
    let podman = list_containers(ContainerRuntime::Podman, None, user);
    let podman_root = list_containers(ContainerRuntime::PodmanRoot, None, user);
    let containerd = containerd_status(user);
    let (docker, podman, podman_root, containerd) =
        tokio::join!(docker, podman, podman_root, containerd);
    Ok(json!({
        "docker": result_value(docker),
        "podman": result_value(podman),
        "podman_root": result_value(podman_root),
        "containerd": result_value(containerd),
    }))
}

fn result_value(result: Result<Value, String>) -> Value {
    result.unwrap_or_else(|error| json!({"available": false, "error": error}))
}

async fn list_containers(
    runtime: ContainerRuntime,
    namespace: Option<&str>,
    user: &UserEnvironment,
) -> Result<Value, String> {
    match runtime {
        ContainerRuntime::Docker => {
            let output = run_runtime(
                runtime,
                &["ps", "-a", "--no-trunc", "--format", "{{json .}}"],
                namespace,
                user,
                QUERY_TIMEOUT,
            )
            .await?;
            let containers = output
                .stdout
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>();
            Ok(list_result(runtime, namespace, containers))
        }
        ContainerRuntime::Podman | ContainerRuntime::PodmanRoot => {
            let output = run_runtime(
                runtime,
                &["ps", "-a", "--no-trunc", "--format", "json"],
                namespace,
                user,
                QUERY_TIMEOUT,
            )
            .await?;
            let containers = serde_json::from_str::<Vec<Value>>(&output.stdout)
                .map_err(|error| format!("parse podman ps JSON: {error}"))?;
            Ok(list_result(runtime, namespace, containers))
        }
        ContainerRuntime::Containerd => {
            let namespace = require_containerd_namespace(namespace)?;
            if nerdctl_path().is_some() {
                let output = run_runtime(
                    runtime,
                    &["ps", "-a", "--no-trunc", "--format", "json"],
                    Some(namespace),
                    user,
                    QUERY_TIMEOUT,
                )
                .await?;
                let containers = parse_json_array_or_lines(&output.stdout);
                Ok(list_result(runtime, Some(namespace), containers))
            } else {
                let output = run_ctr(
                    &["--namespace", namespace, "containers", "list"],
                    QUERY_TIMEOUT,
                )
                .await?;
                Ok(json!({
                    "available": true,
                    "runtime": runtime.as_str(),
                    "namespace": namespace,
                    "format": "ctr-text",
                    "raw": output.stdout,
                }))
            }
        }
    }
}

fn list_result(
    runtime: ContainerRuntime,
    namespace: Option<&str>,
    containers: Vec<Value>,
) -> Value {
    let count = containers.len();
    json!({
        "available": true,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "containers": containers,
        "count": count,
    })
}

async fn containerd_status(user: &UserEnvironment) -> Result<Value, String> {
    let ctr = ctr_path().ok_or_else(|| "ctr is not installed".to_string())?;
    let output = run_root(ctr, &["namespaces", "list"], QUERY_TIMEOUT).await?;
    let namespaces = output
        .stdout
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|value| !value.is_empty())
        .take(32)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut inventories = Vec::new();
    for namespace in &namespaces {
        inventories.push(result_value(
            list_containers(ContainerRuntime::Containerd, Some(namespace), user).await,
        ));
    }
    Ok(json!({
        "available": true,
        "runtime": "containerd",
        "namespaces": namespaces,
        "inventories": inventories,
        "nerdctl": nerdctl_path().is_some(),
    }))
}

async fn inspect_container(
    runtime: ContainerRuntime,
    target: &str,
    namespace: Option<&str>,
    user: &UserEnvironment,
) -> Result<Value, String> {
    require_runtime_cli(runtime)?;
    let output = run_runtime(
        runtime,
        &["inspect", target],
        namespace,
        user,
        QUERY_TIMEOUT,
    )
    .await?;
    let data = serde_json::from_str::<Value>(&output.stdout)
        .map_err(|error| format!("parse container inspect JSON: {error}"))?;
    Ok(json!({
        "available": true,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "target": target,
        "data": data,
    }))
}

async fn container_logs(
    runtime: ContainerRuntime,
    target: &str,
    namespace: Option<&str>,
    lines: u64,
    user: &UserEnvironment,
) -> Result<Value, String> {
    require_runtime_cli(runtime)?;
    let lines = lines.to_string();
    let output = run_runtime(
        runtime,
        &["logs", "--tail", &lines, "--timestamps", target],
        namespace,
        user,
        QUERY_TIMEOUT,
    )
    .await?;
    Ok(json!({
        "available": true,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "target": target,
        "lines": lines,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
    }))
}

async fn container_processes(
    runtime: ContainerRuntime,
    target: &str,
    namespace: Option<&str>,
    user: &UserEnvironment,
) -> Result<Value, String> {
    require_runtime_cli(runtime)?;
    let output = match runtime {
        ContainerRuntime::Podman | ContainerRuntime::PodmanRoot => {
            run_runtime(runtime, &["top", target], namespace, user, QUERY_TIMEOUT).await?
        }
        _ => {
            run_runtime(
                runtime,
                &["top", target, "-eo", "pid,ppid,user,stat,lstart,cmd"],
                namespace,
                user,
                QUERY_TIMEOUT,
            )
            .await?
        }
    };
    Ok(json!({
        "available": true,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "target": target,
        "raw": output.stdout,
    }))
}

async fn container_stats(
    runtime: ContainerRuntime,
    target: &str,
    namespace: Option<&str>,
    user: &UserEnvironment,
) -> Result<Value, String> {
    require_runtime_cli(runtime)?;
    let args = match runtime {
        ContainerRuntime::Docker => vec!["stats", "--no-stream", "--format", "{{json .}}", target],
        _ => vec!["stats", "--no-stream", "--format", "json", target],
    };
    let output = run_runtime(runtime, &args, namespace, user, QUERY_TIMEOUT).await?;
    Ok(json!({
        "available": true,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "target": target,
        "data": parse_json_array_or_lines(&output.stdout),
        "raw": output.stdout,
    }))
}

async fn namespace_report(
    runtime: ContainerRuntime,
    target: &str,
    namespace: Option<&str>,
    user: &UserEnvironment,
) -> Result<Value, String> {
    let inspect = inspect_container(runtime, target, namespace, user).await?;
    let pid = inspect["data"]
        .as_array()
        .and_then(|values| values.first())
        .and_then(|value| value.pointer("/State/Pid"))
        .or_else(|| inspect["data"].pointer("/State/Pid"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "container is not running or inspect has no State.Pid".to_string())?;
    if pid == 0 {
        return Err("container is not running".to_string());
    }
    let start_time = crate::proc::read_start_time_ticks_pub(pid)
        .ok_or_else(|| format!("container init pid disappeared: {pid}"))?;
    let mut namespaces = serde_json::Map::new();
    for name in [
        "cgroup",
        "ipc",
        "mnt",
        "net",
        "pid",
        "pid_for_children",
        "time",
        "time_for_children",
        "user",
        "uts",
    ] {
        if let Ok(target) = fs::read_link(format!("/proc/{pid}/ns/{name}")) {
            namespaces.insert(
                name.to_string(),
                Value::String(target.to_string_lossy().into_owned()),
            );
        }
    }
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| format!("read container init status: {error}"))?;
    let status_fields = status
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| {
            matches!(
                *key,
                "Name"
                    | "State"
                    | "Uid"
                    | "Gid"
                    | "NSpid"
                    | "NoNewPrivs"
                    | "Seccomp"
                    | "CapInh"
                    | "CapPrm"
                    | "CapEff"
                    | "CapBnd"
                    | "CapAmb"
            )
        })
        .map(|(key, value)| (key.to_string(), value.trim().to_string()))
        .map(|(key, value)| (key, Value::String(value)))
        .collect::<serde_json::Map<_, _>>();
    let cgroups = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|error| format!("read container cgroups: {error}"))?;
    if crate::proc::read_start_time_ticks_pub(pid) != Some(start_time) {
        return Err("container init pid changed during namespace inspection".to_string());
    }
    Ok(json!({
        "available": true,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "target": target,
        "pid": pid,
        "pid_start_time_ticks": start_time,
        "namespaces": namespaces,
        "status": status_fields,
        "cgroups": cgroups.lines().collect::<Vec<_>>(),
    }))
}

async fn mutate(
    action: &str,
    runtime: ContainerRuntime,
    target: &str,
    namespace: Option<&str>,
    signal: Option<&str>,
    confirm: bool,
    user: &UserEnvironment,
) -> Result<Value, String> {
    require_runtime_cli(runtime)?;
    let before = inspect_container(runtime, target, namespace, user)
        .await
        .ok();
    let args = match action {
        "start" | "stop" | "restart" | "pause" | "unpause" => vec![action, target],
        "kill" => vec!["kill", "--signal", signal.unwrap(), target],
        "remove" => {
            if !confirm {
                return Err("remove requires confirm=true".to_string());
            }
            vec!["rm", target]
        }
        _ => unreachable!("validated container mutation"),
    };
    let output = run_runtime(runtime, &args, namespace, user, CONTROL_TIMEOUT).await?;
    let after = inspect_container(runtime, target, namespace, user).await;
    let after = match after {
        Ok(after) => after,
        Err(error) if action == "remove" => json!({
            "present": false,
            "error": error,
        }),
        Err(error) => {
            return Ok(json!({
                "action": action,
                "runtime": runtime.as_str(),
                "namespace": namespace,
                "target": target,
                "changed": Value::Null,
                "action_applied": true,
                "before": before,
                "stdout_tail": tail(&output.stdout),
                "stderr_tail": tail(&output.stderr),
                "post_state_error": error,
            }));
        }
    };
    Ok(json!({
        "action": action,
        "runtime": runtime.as_str(),
        "namespace": namespace,
        "target": target,
        "changed": before.as_ref() != Some(&after),
        "action_applied": true,
        "before": before,
        "after": after,
        "stdout_tail": tail(&output.stdout),
        "stderr_tail": tail(&output.stderr),
        "reversible": matches!(action, "start" | "stop" | "pause" | "unpause"),
        "inverse_action": match action {
            "start" => Some("stop"),
            "stop" => Some("start"),
            "pause" => Some("unpause"),
            "unpause" => Some("pause"),
            _ => None,
        },
    }))
}

fn require_runtime_cli(runtime: ContainerRuntime) -> Result<(), String> {
    if runtime == ContainerRuntime::Containerd && nerdctl_path().is_none() {
        return Err("containerd lifecycle/log/inspect requires nerdctl".to_string());
    }
    runtime_program(runtime).map(|_| ())
}

fn require_containerd_namespace(namespace: Option<&str>) -> Result<&str, String> {
    namespace.ok_or_else(|| "containerd operations require a namespace".to_string())
}

async fn run_runtime(
    runtime: ContainerRuntime,
    args: &[&str],
    namespace: Option<&str>,
    user: &UserEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let program = runtime_program(runtime)?;
    let mut owned = Vec::new();
    if runtime == ContainerRuntime::Containerd {
        let namespace = require_containerd_namespace(namespace)?;
        owned.push("--namespace".to_string());
        owned.push(namespace.to_string());
    }
    owned.extend(args.iter().map(|value| value.to_string()));
    run_command(program, owned, runtime.identity(user)?, timeout).await
}

async fn run_ctr(args: &[&str], timeout: Duration) -> Result<CommandOutput, String> {
    let ctr = ctr_path().ok_or_else(|| "ctr is not installed".to_string())?;
    run_root(ctr, args, timeout).await
}

async fn run_root(
    program: &'static str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    run_command(
        program,
        args.iter().map(|value| value.to_string()).collect(),
        None,
        timeout,
    )
    .await
}

fn runtime_program(runtime: ContainerRuntime) -> Result<&'static str, String> {
    match runtime {
        ContainerRuntime::Docker => tool_path(&["/usr/bin/docker", "/bin/docker"])
            .ok_or_else(|| "docker is not installed".to_string()),
        ContainerRuntime::Podman | ContainerRuntime::PodmanRoot => {
            tool_path(&["/usr/bin/podman", "/bin/podman"])
                .ok_or_else(|| "podman is not installed".to_string())
        }
        ContainerRuntime::Containerd => {
            nerdctl_path().ok_or_else(|| "nerdctl is not installed".to_string())
        }
    }
}

fn nerdctl_path() -> Option<&'static str> {
    tool_path(&["/usr/bin/nerdctl", "/usr/local/bin/nerdctl"])
}

fn ctr_path() -> Option<&'static str> {
    tool_path(&["/usr/bin/ctr", "/usr/local/bin/ctr"])
}

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

fn parse_json_array_or_lines(output: &str) -> Vec<Value> {
    if let Ok(values) = serde_json::from_str::<Vec<Value>>(output.trim()) {
        return values;
    }
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn validate_action(
    action: &str,
    runtime: Option<&str>,
    target: Option<&str>,
    namespace: Option<&str>,
    signal: Option<&str>,
    lines: Option<u64>,
    confirm: bool,
) -> Result<(), String> {
    match action {
        "status"
            if runtime.is_none()
                && target.is_none()
                && namespace.is_none()
                && signal.is_none()
                && lines.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "list"
            if runtime.is_some()
                && target.is_none()
                && signal.is_none()
                && lines.is_none()
                && !confirm =>
        {
            validate_namespace_requirement(runtime, namespace)
        }
        "inspect" | "processes" | "stats" | "namespaces"
            if runtime.is_some()
                && target.is_some()
                && signal.is_none()
                && lines.is_none()
                && !confirm =>
        {
            validate_namespace_requirement(runtime, namespace)
        }
        "logs"
            if runtime.is_some()
                && target.is_some()
                && signal.is_none()
                && lines.unwrap_or(100) > 0
                && lines.unwrap_or(100) <= MAX_LOG_LINES
                && !confirm =>
        {
            validate_namespace_requirement(runtime, namespace)
        }
        "start" | "stop" | "restart" | "pause" | "unpause"
            if runtime.is_some()
                && target.is_some()
                && signal.is_none()
                && lines.is_none()
                && !confirm =>
        {
            validate_namespace_requirement(runtime, namespace)
        }
        "kill"
            if runtime.is_some()
                && target.is_some()
                && signal.is_some_and(valid_signal)
                && lines.is_none()
                && !confirm =>
        {
            validate_namespace_requirement(runtime, namespace)
        }
        "remove"
            if runtime.is_some()
                && target.is_some()
                && signal.is_none()
                && lines.is_none()
                && confirm =>
        {
            validate_namespace_requirement(runtime, namespace)
        }
        _ => Err(format!("invalid arguments for container action {action:?}")),
    }
}

fn validate_namespace_requirement(
    runtime: Option<&str>,
    namespace: Option<&str>,
) -> Result<(), String> {
    match runtime {
        Some("containerd") if namespace.is_none() => {
            Err("containerd operations require --namespace".to_string())
        }
        Some("docker" | "podman" | "podman-root") if namespace.is_some() => {
            Err("only containerd accepts --namespace".to_string())
        }
        Some("docker" | "podman" | "podman-root" | "containerd") => Ok(()),
        Some(other) => Err(format!("unknown container runtime: {other}")),
        None => Err("container runtime is required".to_string()),
    }
}

fn is_mutating(action: &str) -> bool {
    matches!(
        action,
        "start" | "stop" | "restart" | "pause" | "unpause" | "kill" | "remove"
    )
}

fn valid_signal(signal: &str) -> bool {
    matches!(signal, "TERM" | "KILL" | "HUP" | "INT" | "USR1" | "USR2")
}

fn validate_identifier(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(format!("invalid {kind}: {value:?}"));
    }
    Ok(())
}

#[derive(Clone)]
struct UserEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    username: String,
}

impl UserEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf) -> Result<Self, String> {
        let metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect container user home {}: {error}", home.display()))?;
        if metadata.uid() != uid {
            return Err(format!(
                "container user home {} belongs to uid {}, expected {uid}",
                home.display(),
                metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        Ok(Self {
            uid,
            gid,
            home,
            runtime_dir,
            username: username_for_uid(uid)?,
        })
    }

    fn validate_runtime(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.runtime_dir).map_err(|error| {
            format!(
                "inspect container user runtime {}: {error}",
                self.runtime_dir.display()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != self.uid {
            return Err(format!(
                "container user runtime {} is not a user-owned directory",
                self.runtime_dir.display()
            ));
        }
        Ok(())
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
    let username = unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map_err(|_| format!("username is not UTF-8 for uid {uid}"))?
        .to_string();
    Ok(username)
}

async fn run_command(
    program: &'static str,
    args: Vec<String>,
    identity: Option<UserEnvironment>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, identity, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    identity: Option<UserEnvironment>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(identity) = &identity {
        command
            .env("HOME", &identity.home)
            .env("USER", &identity.username)
            .env("LOGNAME", &identity.username)
            .env("XDG_RUNTIME_DIR", &identity.runtime_dir);
    } else {
        command.env("HOME", "/root");
    }
    let drop_identity = identity.map(|identity| (identity.uid, identity.gid));
    unsafe {
        command.pre_exec(move || {
            if let Some((uid, gid)) = drop_identity {
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
            if drop_identity.is_none() && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
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
    if !output.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            program,
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    Ok(output)
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read container command output: {error}"))?;
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
    use super::*;

    #[test]
    fn container_identifiers_reject_options_and_globs() {
        validate_identifier("container", "web-1").unwrap();
        assert!(validate_identifier("container", "--privileged").is_err());
        assert!(validate_identifier("container", "*").is_err());
    }

    #[test]
    fn containerd_requires_namespace() {
        assert!(validate_namespace_requirement(Some("containerd"), None).is_err());
        validate_namespace_requirement(Some("containerd"), Some("k8s.io")).unwrap();
    }
}

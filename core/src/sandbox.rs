//! The agent's sandbox tool.
//!
//! Exposed only as `cos_sandbox` in
//! [`crate::agent::tools::cos_proxy`], not as a user-facing CLI
//! primitive. The agent uses it to run model-generated or otherwise
//! untrusted commands.
//!
//! There is no isolation implementation here. A model-authored command
//! is exactly as hostile as a third-party App operation, so it is
//! derived into the same [`crate::worker`] launch policy and enforced
//! by the same provider. What this module still owns is the tool's
//! argument surface: parsing, validating and capability-checking the
//! request *before* any of it becomes policy.
//!
//! Only one operation is supported: `exec`. Persistent sandboxes
//! (create/destroy/list) were a legacy surface area; they never
//! spawned a real init process and have been removed.
//!
//! Unsupported platforms and missing isolation primitives fail closed.

use serde_json::{json, Value};

use crate::caps::{require_or_json, Scope, Verb};
use crate::worker::{Endpoint, Limits};

const DEFAULT_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_CPU_PERCENT: u32 = 100;
const DEFAULT_PIDS_MAX: u32 = 64;
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: u64 = 1_048_576;

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "exec" => cmd_exec(args),
        _ => Err(format!("unknown sandbox command: {command}")),
    }
}

/// Everything the tool accepted, before it becomes a launch policy.
#[derive(Debug)]
struct ExecRequest {
    read_only: bool,
    workspace: String,
    endpoints: Vec<Endpoint>,
    limits: Limits,
    command: Vec<String>,
}

/// Execute a command in an isolated sandbox.
///
/// Args mirror the agent tool schema in
/// `agent::tools::cos_proxy::PRIMITIVES`:
///   [--allow-host HOST:PORT]... [--rw] [--workspace DIR]
///   [--mem LIMIT] [--cpu PERCENT] [--pids MAX] [--timeout SECS]
///   -- <command> [args...]
fn cmd_exec(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SPAWN, Scope::wild()).map_err(|v| v.to_string())?;
    let request = parse_exec(args)?;

    let workspace_scope = Scope::path(format!("{}/**", request.workspace.trim_end_matches('/')));
    require_or_json(Verb::FS_READ, workspace_scope.clone()).map_err(|value| value.to_string())?;
    if !request.read_only {
        require_or_json(Verb::FS_WRITE, workspace_scope).map_err(|value| value.to_string())?;
    }
    for endpoint in &request.endpoints {
        require_or_json(Verb::NET_DIAL, Scope::host(endpoint.authority()))
            .map_err(|value| value.to_string())?;
    }
    exec_sandboxed(request)
}

/// Parse the tool arguments.
///
/// Nothing here reaches the sandbox directly: the values are
/// validated, then handed to the trusted derivation that builds the
/// policy. In particular the command vector never becomes an argv the
/// provider trusts — the derivation resolves the program itself.
fn parse_exec(args: &[String]) -> Result<ExecRequest, String> {
    let mut read_only = true;
    let mut workspace = crate::paths::current_home_override()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| crate::config::get().home.clone());
    let mut endpoints: Vec<Endpoint> = Vec::new();
    let mut limits = Limits::operation();
    limits.memory_bytes = DEFAULT_MEMORY_BYTES;
    limits.cpu_percent = DEFAULT_CPU_PERCENT;
    limits.pids_max = DEFAULT_PIDS_MAX;
    limits.runtime = std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    limits.output_bytes = MAX_OUTPUT_BYTES;
    let mut cmd_start = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-network" => {
                endpoints.clear();
                i += 1;
            }
            "--network" => {
                // Unrestricted egress cannot be enforced: the broker has
                // no host identity to pin, so there is nothing to check
                // a connection against.
                return Err(
                    "sandbox egress requires explicit `--allow-host HOST:PORT` endpoints"
                        .to_string(),
                );
            }
            "--allow-host" if i + 1 < args.len() => {
                endpoints.push(parse_endpoint(&args[i + 1])?);
                i += 2;
            }
            "--ro" => {
                read_only = true;
                i += 1;
            }
            "--rw" => {
                read_only = false;
                i += 1;
            }
            "--workspace" if i + 1 < args.len() => {
                workspace = args[i + 1].clone();
                i += 2;
            }
            "--mem" if i + 1 < args.len() => {
                limits.memory_bytes = parse_memory_limit(&args[i + 1])?;
                i += 2;
            }
            "--cpu" if i + 1 < args.len() => {
                let percent = args[i + 1]
                    .parse::<u32>()
                    .map_err(|_| format!("invalid cpu value: {}", args[i + 1]))?;
                if !matches!(percent, 1..=100) {
                    return Err("cpu percent must be between 1 and 100".to_string());
                }
                limits.cpu_percent = percent;
                i += 2;
            }
            "--pids" if i + 1 < args.len() => {
                let pids = args[i + 1]
                    .parse::<u32>()
                    .map_err(|_| format!("invalid pids value: {}", args[i + 1]))?;
                if !matches!(pids, 1..=1024) {
                    return Err("pids limit must be between 1 and 1024".to_string());
                }
                limits.pids_max = pids;
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                let secs = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| format!("invalid timeout value: {}", args[i + 1]))?;
                if !matches!(secs, 1..=3600) {
                    return Err("timeout must be between 1 and 3600 seconds".to_string());
                }
                limits.runtime = std::time::Duration::from_secs(secs);
                i += 2;
            }
            "--seccomp-profile" if i + 1 < args.len() => {
                // The profile is no longer selectable: every hostile
                // worker gets the same filter, and the only variation
                // is whether brokered egress was granted. The flag is
                // still accepted so an older tool call does not fail.
                let profile = args[i + 1].to_lowercase();
                if !["minimal", "network", "full", "strict"].contains(&profile.as_str()) {
                    return Err("seccomp profile must be: minimal, network, full".into());
                }
                i += 2;
            }
            "--" => {
                cmd_start = Some(i + 1);
                break;
            }
            _ => {
                cmd_start = Some(i);
                break;
            }
        }
    }

    let cmd_idx = cmd_start.ok_or("no command specified")?;
    if cmd_idx >= args.len() {
        return Err("no command specified".into());
    }
    Ok(ExecRequest {
        read_only,
        workspace,
        endpoints,
        limits,
        command: args[cmd_idx..].to_vec(),
    })
}

fn parse_endpoint(value: &str) -> Result<Endpoint, String> {
    let (host, port) = value
        .rsplit_once(':')
        .ok_or_else(|| format!("--allow-host needs HOST:PORT, got `{value}`"))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| format!("invalid port in `{value}`"))?;
    let endpoint = Endpoint::new(host, port);
    crate::worker::policy::validate_endpoint(&endpoint)?;
    Ok(endpoint)
}

fn parse_memory_limit(value: &str) -> Result<u64, String> {
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b'K' | b'k') => (&value[..value.len() - 1], 1024_u64),
        Some(b'M' | b'm') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'G' | b'g') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return Err("memory limit must not be empty".to_string()),
    };
    let bytes = number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| format!("invalid memory limit: {value}"))?;
    if !(16 * 1024 * 1024..=4 * 1024 * 1024 * 1024).contains(&bytes) {
        return Err("memory limit must be between 16M and 4G".to_string());
    }
    Ok(bytes)
}

/// Derive the launch policy and run it through the shared provider.
fn exec_sandboxed(request: ExecRequest) -> Result<Value, String> {
    let workspace = std::fs::canonicalize(&request.workspace)
        .map_err(|error| format!("invalid sandbox workspace: {error}"))?;
    if !workspace.is_dir() {
        return Err("sandbox workspace must be a directory".to_string());
    }
    if let Some(home) = crate::paths::current_home_override() {
        let home = home
            .canonicalize()
            .map_err(|error| format!("invalid sandbox owner home: {error}"))?;
        if !workspace.starts_with(&home) {
            return Err(format!(
                "sandbox workspace {} escapes owner home {}",
                workspace.display(),
                home.display()
            ));
        }
    }
    let limits = request.limits;
    let read_only = request.read_only;
    let policy = crate::worker::derive::agent_exec(crate::worker::derive::AgentExecInput {
        workspace: workspace.clone(),
        writable: !read_only,
        argv: request.command,
        endpoints: request.endpoints,
        limits,
    })
    .inspect_err(|error| {
        crate::worker::audit::refused(
            "agent:exec",
            crate::worker::TrustTier::AgentExec.as_str(),
            error,
        );
    })?;
    let network_mode = policy.network.as_str();
    let launch = crate::worker::WorkerLaunch::new(policy);
    let prepared = crate::worker::prepare(&launch).inspect_err(|error| {
        crate::worker::audit::refused(
            "agent:exec",
            crate::worker::TrustTier::AgentExec.as_str(),
            error,
        );
    })?;
    let facts = prepared.facts.clone();
    let governor = prepared.governor;
    crate::worker::audit::launched(&facts, None);

    let output = crate::worker::run_captured(prepared, None, limits, |_| Ok(()))?;
    crate::worker::audit::outcome(
        facts["policy"].as_str().unwrap_or_default(),
        "agent:exec",
        output.audit_facts(),
    );

    let exit_code = output.status.code().unwrap_or(-1);
    let mut result = json!({
        "exit_code": exit_code,
        "stdout": output.stdout_string(),
        "stderr": output.stderr_string(),
        "isolated": true,
        "network": network_mode,
        "read_only_root": true,
        "workspace_read_only": read_only,
        "workspace": workspace.to_string_lossy(),
        "governor": governor.as_str(),
        "policy": facts["policy"].clone(),
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
        "limits": {
            "memory": limits.memory_bytes,
            "cpu_percent": limits.cpu_percent,
            "pids_max": limits.pids_max,
            "timeout_secs": limits.runtime.as_secs(),
        },
        "seccomp_profile": facts["seccomp"].clone(),
    });
    if output.timed_out {
        result["killed_by"] = json!("timeout");
    } else if exit_code == 137 {
        result["killed_by"] = json!("OOM (memory limit exceeded)");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/sandbox.rs"));
}

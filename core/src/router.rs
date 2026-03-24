use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::apps;
use crate::audit;
use crate::bridge;
use crate::browser;
use crate::checkpoint;
use crate::credential;
use crate::ipc;
use crate::netfilter;
use crate::policy;
use crate::proc;
use crate::sandbox;
use crate::service;
use crate::sysinfo;
use crate::watch;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn apps_dir() -> PathBuf {
    PathBuf::from(env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir() -> String {
    env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into())
}

fn audit_path() -> PathBuf {
    Path::new(&data_dir()).join("logs").join("audit.jsonl")
}

/// Main dispatch: parse CLI args and route to the appropriate handler.
pub fn dispatch(args: &[String]) -> Result<Option<String>, String> {
    if args.is_empty() {
        return show_apps();
    }

    let app_name = &args[0];
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);

    // Check if it's a known app
    if !discovered.contains_key(app_name.as_str()) {
        // Check built-in apps
        match app_name.as_str() {
            "sys" => return dispatch_builtin(args, "sys", sysinfo::run),
            "sandbox" => return dispatch_builtin(args, "sandbox", sandbox::run),
            "proc" => return dispatch_builtin(args, "proc", proc::run),
            "ipc" => return dispatch_builtin(args, "ipc", ipc::run),
            "browser" => return dispatch_builtin(args, "browser", browser::run),
            "service" => return dispatch_builtin(args, "service", service::run),
            "watch" => return dispatch_builtin(args, "watch", watch::run),
            "checkpoint" => return dispatch_builtin(args, "checkpoint", checkpoint::run),
            "credential" => return dispatch_builtin(args, "credential", credential::run),
            "netfilter" => return dispatch_builtin(args, "netfilter", netfilter::run),
            "policy" => return dispatch_builtin(args, "policy", policy::run),
            _ => {}
        }
        let names: Vec<&String> = discovered.keys().collect();
        return Err(format!("unknown app: {app_name}. installed: {names:?}"));
    }

    // One arg: show app help
    if args.len() == 1 {
        return show_app_help(app_name, &discovered[app_name.as_str()]);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let app = &discovered[app_name.as_str()];

    // Validate command exists
    if !app.manifest.commands.contains_key(command.as_str()) {
        let valid: Vec<&String> = app.manifest.commands.keys().collect();
        return Err(format!(
            "unknown command: {app_name} {command}. available: {valid:?}"
        ));
    }

    run_app_command(app_name, command, &cmd_args, app)
}

fn show_apps() -> Result<Option<String>, String> {
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);

    let mut app_list = Vec::new();
    for (name, app) in &discovered {
        app_list.push(json!({
            "name": name,
            "description": app.manifest.description,
            "commands": app.manifest.commands,
        }));
    }
    // Always include built-in apps
    for (name, desc, cmds) in builtin_apps() {
        let cmd_map: serde_json::Map<String, Value> = cmds
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        app_list.push(json!({
            "name": name,
            "description": desc,
            "commands": cmd_map,
        }));
    }

    let output = json!({
        "name": "cos",
        "version": VERSION,
        "description": "Claw OS — agent-native operating system. All commands return structured JSON.",
        "apps": app_list,
        "total_apps": app_list.len(),
        "hint": "Run: cos <app> for app details, cos <app> <command> [args] to execute.",
    });
    Ok(Some(output.to_string()))
}

fn show_app_help(name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let output = json!({
        "app": name,
        "version": app.manifest.version,
        "description": app.manifest.description,
        "commands": app.manifest.commands,
        "hint": format!("Run: cos {name} <command> [args]"),
    });
    Ok(Some(output.to_string()))
}

fn run_app_command(
    app_name: &str,
    command: &str,
    args: &[String],
    app: &apps::App,
) -> Result<Option<String>, String> {
    let start = Instant::now();
    let audit = audit_path();
    let data = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    let result = bridge::run_python_app(&app.dir, command, args, &data, &apps);

    match result {
        Ok(output) => {
            let mut status = "ok";
            let err_string;
            let mut error_msg: Option<&str> = None;

            // Check if the output contains an error key
            if let Some(ref s) = output {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    if let Some(e) = v["error"].as_str() {
                        status = "error";
                        err_string = e.to_string();
                        error_msg = Some(&err_string);
                    }
                }
            }

            audit::log_entry(&audit, app_name, command, args, start, status, error_msg);
            Ok(output)
        }
        Err(e) => {
            audit::log_entry(&audit, app_name, command, args, start, "error", Some(&e));
            // Enrich error with recovery hints for agents
            if let Some(recovery) = recovery_hint(&e) {
                Ok(Some(
                    json!({
                        "error": e,
                        "recovery": recovery,
                    })
                    .to_string(),
                ))
            } else {
                Err(e)
            }
        }
    }
}

fn builtin_apps() -> Vec<(
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
)> {
    vec![
        ("sys", "System information — hardware, OS, environment, resources, structured /proc", vec![
            ("info", "Get OS, architecture, hostname, and version info"),
            ("env", "List environment variables, optionally filter by pattern"),
            ("resources", "Show disk, memory, and CPU usage"),
            ("uptime", "Show system uptime"),
            ("proc", "List all processes with PID, name, state, CPU, memory (structured /proc/*/stat)"),
            ("mounts", "List all mount points with filesystem type and options (structured /proc/mounts)"),
            ("net", "Show network interfaces and TCP connections (structured /proc/net/*)"),
            ("cgroup", "Show cgroup v2 limits and usage — memory, CPU, PIDs (/sys/fs/cgroup/)"),
        ]),
        ("sandbox", "Lightweight process isolation using Linux namespaces + cgroup v2 + seccomp", vec![
            ("exec", "Run a command in an isolated namespace (--mem, --cpu, --pids, --timeout, --seccomp-profile minimal|network|full)"),
            ("create", "Create a persistent sandbox configuration"),
            ("destroy", "Remove a sandbox by ID"),
            ("list", "List all active sandboxes"),
        ]),
        ("proc", "Process session manager — spawn, track, control, and monitor processes", vec![
            ("spawn", "Start a process (--session ID, --group NAME, --priority low|normal|high|realtime, --tier N, --scope PATH)"),
            ("status", "Check if a session's process is still running"),
            ("output", "Read buffered stdout/stderr (--tail N, --follow, --since-offset BYTES)"),
            ("kill", "Terminate a session's process or an entire --group"),
            ("list", "List all sessions, optionally filter by --group"),
            ("wait", "Block until a process exits, return exit status and output"),
            ("signal", "Send a Unix signal (TERM, KILL, HUP, USR1, USR2, STOP, CONT)"),
            ("result", "Get full exit report with heuristic success detection"),
            ("stats", "Get resource usage stats — CPU time, memory, I/O bytes, threads (from /proc/<pid>/)"),
            ("renice", "Change process priority (--priority low|normal|high|realtime)"),
        ]),
        ("ipc", "Inter-process communication — messages, locks, barriers", vec![
            ("send", "Queue a message to a target session"),
            ("recv", "Dequeue oldest message from a session (--timeout N, --peek)"),
            ("list", "Show all queued messages for a session"),
            ("clear", "Delete all messages for a session"),
            ("lock", "Acquire a named mutex lock (--holder, --timeout)"),
            ("unlock", "Release a named mutex lock"),
            ("locks", "List all active locks"),
            ("barrier", "Wait until N sessions reach a synchronization point (--expect N, --session ID)"),
        ]),
        ("browser", "Browser-as-a-service — Jina Reader lifecycle control", vec![
            ("start", "Start the Jina Reader browser service"),
            ("stop", "Stop the browser service"),
            ("restart", "Restart the browser service"),
            ("status", "Check if browser service is running and healthy"),
            ("health", "Run health check, auto-restart on failure"),
        ]),
        ("service", "Generic service manager — discover, start, stop, health-check any service", vec![
            ("start", "Start a registered service by name"),
            ("stop", "Stop a running service"),
            ("restart", "Restart a service (stop then start)"),
            ("status", "Check service running/healthy state with log tail"),
            ("health", "Run health check, optionally auto-restart (--no-restart to skip)"),
            ("list", "List all discovered services with status"),
            ("logs", "View service log output (--tail N)"),
            ("register", "Register a new service (--name, --command, --workdir, --health-url)"),
        ]),
        ("watch", "Event watcher — block until file, directory, process, or OS event changes", vec![
            ("file", "Watch a file for creation, modification, or deletion (--timeout N)"),
            ("dir", "Watch a directory for any file changes (--timeout N)"),
            ("proc", "Watch a process session for exit (--timeout N)"),
            ("on", "Subscribe to OS events: proc.exit, fs.change, service.health-fail, checkpoint.created, quota.exceeded"),
        ]),
        ("checkpoint", "OverlayFS checkpoint system — snapshot, diff, rollback, quota, namespaces", vec![
            ("create", "Freeze current changes into a named checkpoint and start fresh"),
            ("diff", "Show created, modified, and deleted files in the current upper layer"),
            ("rollback", "Restore a checkpoint or reset to base (wipe current changes)"),
            ("list", "List all saved checkpoints with metadata"),
            ("status", "Show overlay mount state, pending changes, and disk usage"),
            ("quota-set", "Set filesystem quota for the upper layer (e.g. 2G, 512M)"),
            ("quota-status", "Show current quota usage, limit, and whether exceeded"),
            ("namespaces", "Manage isolated overlay namespaces (--create, --destroy, --status <name>)"),
        ]),
        ("credential", "Encrypted credential store — secure secret storage with tier-based access", vec![
            ("store", "Store a credential (cos credential store <name> <value> [--tier N])"),
            ("load", "Load a credential value (tier check enforced)"),
            ("revoke", "Delete a stored credential"),
            ("list", "List all credentials (names and metadata only, never values)"),
        ]),
        ("netfilter", "Outbound network firewall — domain, method, path, and binary-level rules", vec![
            ("add", "Add a rule (--allow|--deny <domain> [--port N] [--method GET,POST] [--path /api/**] [--binary /usr/bin/git] [--tls])"),
            ("remove", "Remove rules for a domain"),
            ("list", "List all rules and default policy"),
            ("check", "Check if a request is allowed (--method M --path P --binary B)"),
            ("reset", "Remove all rules and reset to allow-all"),
            ("default", "Set default policy (allow-all or deny-all)"),
            ("export", "Export full ruleset as JSON for proxy consumption"),
        ]),
        ("policy", "Permission system — tier/scope checks, temporary elevation", vec![
            ("elevate", "Temporarily elevate session tier (--to N --duration SECS --reason TEXT)"),
            ("drop", "Drop an active elevation"),
            ("status", "Show current session tier, elevation, and allowed operations"),
            ("check", "Check if a specific operation (read/write/exec/net/system) is allowed"),
        ]),
    ]
}

/// Suggest recovery actions for common errors.
/// Agent-native: humans debug by intuition, agents need explicit guidance.
fn recovery_hint(error: &str) -> Option<serde_json::Value> {
    let err_lower = error.to_lowercase();

    if err_lower.contains("permission denied") || err_lower.contains("eperm") {
        return Some(json!({
            "hint": "Permission denied. Check file permissions.",
            "try": ["cos exec run 'ls -la <path>'", "cos exec run 'chmod +rw <path>'"],
        }));
    }
    if err_lower.contains("no such file")
        || err_lower.contains("enoent")
        || err_lower.contains("not found")
    {
        return Some(json!({
            "hint": "File or command not found. Verify the path exists.",
            "try": ["cos fs ls <parent-directory>", "cos exec which <command>"],
        }));
    }
    if err_lower.contains("no space left") || err_lower.contains("enospc") {
        return Some(json!({
            "hint": "Disk full. Free space before retrying.",
            "try": ["cos sys resources", "cos exec run 'du -sh /den/* | sort -rh | head'"],
        }));
    }
    if err_lower.contains("connection refused") || err_lower.contains("econnrefused") {
        return Some(json!({
            "hint": "Connection refused. The target service may not be running.",
            "try": ["cos service list", "cos service start <service-name>"],
        }));
    }
    if err_lower.contains("timed out") || err_lower.contains("timeout") {
        return Some(json!({
            "hint": "Operation timed out. Consider increasing timeout or checking if the service is responsive.",
            "try": ["cos proc list", "cos sys resources"],
        }));
    }
    if err_lower.contains("already running")
        || err_lower.contains("address already in use")
        || err_lower.contains("eaddrinuse")
    {
        return Some(json!({
            "hint": "Port/resource already in use. Another process may be occupying it.",
            "try": ["cos proc list", "cos exec run 'lsof -i :<port>'"],
        }));
    }
    if err_lower.contains("out of memory")
        || err_lower.contains("enomem")
        || err_lower.contains("oom")
    {
        return Some(json!({
            "hint": "Out of memory. Reduce workload or increase memory limits.",
            "try": ["cos sys resources", "cos proc list"],
        }));
    }

    None
}

fn dispatch_builtin(
    args: &[String],
    app_name: &str,
    handler: fn(&str, &[String]) -> Result<Value, String>,
) -> Result<Option<String>, String> {
    if args.len() == 1 {
        let apps = builtin_apps();
        let app = apps.iter().find(|(n, _, _)| *n == app_name).unwrap();
        let cmds: serde_json::Map<String, Value> = app
            .2
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        let output = json!({
            "app": app_name,
            "description": app.1,
            "commands": cmds,
            "hint": format!("Run: cos {app_name} <command> [args]"),
        });
        return Ok(Some(output.to_string()));
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let start = std::time::Instant::now();
    let audit_p = audit_path();

    let result = handler(command, &cmd_args);

    match &result {
        Ok(v) => {
            audit::log_entry(&audit_p, app_name, command, &cmd_args, start, "ok", None);
            Ok(Some(v.to_string()))
        }
        Err(e) => {
            audit::log_entry(
                &audit_p,
                app_name,
                command,
                &cmd_args,
                start,
                "error",
                Some(e),
            );
            // Enrich error with recovery hints for agents
            if let Some(recovery) = recovery_hint(e) {
                Ok(Some(
                    json!({
                        "error": e.to_string(),
                        "recovery": recovery,
                    })
                    .to_string(),
                ))
            } else {
                Err(e.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_hint_permission_denied() {
        let hint = recovery_hint("Permission denied on /den/file.txt").unwrap();
        assert_eq!(hint["hint"], "Permission denied. Check file permissions.");
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("chmod")));
    }

    #[test]
    fn recovery_hint_eperm_variant() {
        let hint = recovery_hint("EPERM: operation not permitted").unwrap();
        assert_eq!(hint["hint"], "Permission denied. Check file permissions.");
    }

    #[test]
    fn recovery_hint_file_not_found() {
        let hint = recovery_hint("No such file or directory: /den/missing").unwrap();
        assert_eq!(
            hint["hint"],
            "File or command not found. Verify the path exists."
        );
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("cos fs ls")));
    }

    #[test]
    fn recovery_hint_enoent_variant() {
        let hint = recovery_hint("ENOENT: cannot open /tmp/data").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn recovery_hint_not_found_variant() {
        let hint = recovery_hint("command not found: foobar").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn recovery_hint_disk_full() {
        let hint = recovery_hint("No space left on device").unwrap();
        assert_eq!(hint["hint"], "Disk full. Free space before retrying.");
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("cos sys resources")));
    }

    #[test]
    fn recovery_hint_enospc_variant() {
        let hint = recovery_hint("ENOSPC: write failed").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Disk full"));
    }

    #[test]
    fn recovery_hint_connection_refused() {
        let hint = recovery_hint("Connection refused to localhost:8080").unwrap();
        assert!(hint["hint"]
            .as_str()
            .unwrap()
            .contains("Connection refused"));
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("cos service")));
    }

    #[test]
    fn recovery_hint_econnrefused_variant() {
        let hint = recovery_hint("ECONNREFUSED: connect failed").unwrap();
        assert!(hint["hint"]
            .as_str()
            .unwrap()
            .contains("Connection refused"));
    }

    #[test]
    fn recovery_hint_timeout() {
        let hint = recovery_hint("Operation timed out after 30s").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("timed out"));
    }

    #[test]
    fn recovery_hint_timeout_variant() {
        let hint = recovery_hint("request timeout").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("timed out"));
    }

    #[test]
    fn recovery_hint_address_in_use() {
        let hint = recovery_hint("address already in use: 0.0.0.0:3000").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("already in use"));
    }

    #[test]
    fn recovery_hint_eaddrinuse_variant() {
        let hint = recovery_hint("EADDRINUSE: bind failed").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("already in use"));
    }

    #[test]
    fn recovery_hint_out_of_memory() {
        let hint = recovery_hint("Out of memory: cannot allocate").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
    }

    #[test]
    fn recovery_hint_enomem_variant() {
        let hint = recovery_hint("ENOMEM: mmap failed").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
    }

    #[test]
    fn recovery_hint_oom_variant() {
        let hint = recovery_hint("process killed by OOM killer").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
    }

    #[test]
    fn recovery_hint_unknown_error_returns_none() {
        assert!(recovery_hint("something completely unexpected happened").is_none());
    }

    #[test]
    fn recovery_hint_empty_string_returns_none() {
        assert!(recovery_hint("").is_none());
    }

    #[test]
    fn recovery_hint_case_insensitive() {
        // Should match regardless of case
        assert!(recovery_hint("PERMISSION DENIED").is_some());
        assert!(recovery_hint("permission denied").is_some());
        assert!(recovery_hint("Permission Denied").is_some());
    }

    #[test]
    fn recovery_hint_returns_valid_json_structure() {
        // Every hint should have both "hint" (string) and "try" (array of strings)
        let test_errors = [
            "permission denied",
            "no such file",
            "no space left",
            "connection refused",
            "timed out",
            "address already in use",
            "out of memory",
        ];
        for error in &test_errors {
            let hint =
                recovery_hint(error).unwrap_or_else(|| panic!("Expected hint for '{}'", error));
            assert!(
                hint["hint"].is_string(),
                "Missing 'hint' string for '{}'",
                error
            );
            assert!(
                hint["try"].is_array(),
                "Missing 'try' array for '{}'",
                error
            );
            let try_arr = hint["try"].as_array().unwrap();
            assert!(!try_arr.is_empty(), "Empty 'try' array for '{}'", error);
            for cmd in try_arr {
                assert!(cmd.is_string(), "Non-string in 'try' array for '{}'", error);
                assert!(
                    cmd.as_str().unwrap().starts_with("cos "),
                    "Recovery command should start with 'cos': {}",
                    cmd
                );
            }
        }
    }
}

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
use crate::cron;
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
        return show_overview();
    }

    let name = &args[0];

    // "app" namespace → route to Python apps
    if name == "app" {
        return dispatch_app(&args[1..]);
    }

    // Built-in OS primitives
    match name.as_str() {
        "sys" => dispatch_builtin(args, "sys", sysinfo::run),
        "sandbox" => dispatch_builtin(args, "sandbox", sandbox::run),
        "proc" => dispatch_builtin(args, "proc", proc::run),
        "ipc" => dispatch_builtin(args, "ipc", ipc::run),
        "browser" => dispatch_builtin(args, "browser", browser::run),
        "service" => dispatch_builtin(args, "service", service::run),
        "watch" => dispatch_builtin(args, "watch", watch::run),
        "checkpoint" => dispatch_builtin(args, "checkpoint", checkpoint::run),
        "credential" => dispatch_builtin(args, "credential", credential::run),
        "netfilter" => dispatch_builtin(args, "netfilter", netfilter::run),
        "policy" => dispatch_builtin(args, "policy", policy::run),
        "cron" => dispatch_builtin(args, "cron", cron::run),
        _ => {
            // Check if user forgot "app" prefix — helpful error
            let apps_dir = apps_dir();
            let discovered = apps::discover(&apps_dir);
            if discovered.contains_key(name.as_str()) {
                Err(format!(
                    "'{name}' is an app, not an OS primitive. Use: cos app {name} <command>"
                ))
            } else {
                let builtins: Vec<&str> = builtin_apps().iter().map(|(n, _, _)| *n).collect();
                Err(format!(
                    "unknown command: {name}. OS primitives: {builtins:?}. For apps: cos app"
                ))
            }
        }
    }
}

/// Dispatch to Python apps under the "cos app" namespace.
fn dispatch_app(args: &[String]) -> Result<Option<String>, String> {
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);

    // "cos app" with no further args → list available apps
    if args.is_empty() {
        return show_apps(&discovered);
    }

    let app_name = &args[0];

    // Check if it's a known app
    if !discovered.contains_key(app_name.as_str()) {
        let names: Vec<&String> = discovered.keys().collect();
        return Err(format!("unknown app: {app_name}. installed: {names:?}"));
    }

    // "cos app <name>" → show app help
    if args.len() == 1 {
        return show_app_help(app_name, &discovered[app_name.as_str()]);
    }

    // cos app <name> --schema → show all command schemas for this app
    if args.len() == 2 && args[1] == "--schema" {
        return show_app_schema(app_name, &discovered[app_name.as_str()]);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let app = &discovered[app_name.as_str()];

    // If --schema is in args, return app command schema
    if cmd_args.contains(&"--schema".to_string()) {
        return show_app_command_schema(app_name, command, app);
    }

    // Validate command exists
    if !app.manifest.commands.contains_key(command.as_str()) {
        let valid: Vec<&String> = app.manifest.commands.keys().collect();
        return Err(format!(
            "unknown command: cos app {app_name} {command}. available: {valid:?}"
        ));
    }

    run_app_command(app_name, command, &cmd_args, app)
}

fn show_overview() -> Result<Option<String>, String> {
    let mut primitives = Vec::new();
    for (name, desc, cmds) in builtin_apps() {
        let cmd_map: serde_json::Map<String, Value> = cmds
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        primitives.push(json!({
            "name": name,
            "description": desc,
            "commands": cmd_map,
        }));
    }

    // Count available apps without listing them
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);
    let app_count = discovered.len();
    let total_primitives = primitives.len();

    let output = json!({
        "name": "cos",
        "version": VERSION,
        "description": "Claw OS — agent-native operating system. All commands return structured JSON.",
        "primitives": primitives,
        "total_primitives": total_primitives,
        "apps_available": app_count,
        "hint": "Run: cos <primitive> <command> for OS operations. Run: cos app to see available apps.",
    });
    Ok(Some(output.to_string()))
}

fn show_apps(
    discovered: &std::collections::BTreeMap<String, apps::App>,
) -> Result<Option<String>, String> {
    let mut app_list = Vec::new();
    for (name, app) in discovered {
        app_list.push(json!({
            "name": name,
            "description": app.manifest.description,
            "commands": app.manifest.commands,
        }));
    }

    let output = json!({
        "apps": app_list,
        "total": app_list.len(),
        "hint": "Run: cos app <name> for app details, cos app <name> <command> [args] to execute.",
    });
    Ok(Some(output.to_string()))
}

fn show_app_help(name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let output = json!({
        "app": name,
        "version": app.manifest.version,
        "description": app.manifest.description,
        "commands": app.manifest.commands,
        "hint": format!("Run: cos app {name} <command> [args]"),
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
                let mut err_output = json!({
                    "error": e,
                    "recovery": recovery,
                });
                if let Some(code) = error_code_from_hint(&e) {
                    err_output["code"] = json!(code);
                }
                Ok(Some(err_output.to_string()))
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
            ("pipe", "Streaming named pipes — create, publish, subscribe, list, destroy (structured NDJSON channels with replay and backpressure)"),
        ]),
        ("browser", "Browser-as-a-service — Jina Reader lifecycle control", vec![
            ("start", "Start the Jina Reader browser service"),
            ("stop", "Stop the browser service"),
            ("restart", "Restart the browser service"),
            ("status", "Check if browser service is running and healthy"),
            ("health", "Run health check, auto-restart on failure"),
        ]),
        ("service", "Generic service manager — lifecycle hooks, graceful shutdown, dependency ordering", vec![
            ("start", "Start a service (pre_start hook → credential injection → spawn → health check → post_start)"),
            ("stop", "Graceful stop: checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop"),
            ("stop-all", "Stop all services in reverse dependency order with graceful shutdown"),
            ("restart", "Restart a service (graceful stop then start)"),
            ("status", "Check service running/healthy state with log tail"),
            ("health", "Run health check, optionally auto-restart (--no-restart to skip)"),
            ("list", "List all discovered services with status"),
            ("logs", "View service log output (--tail N)"),
            ("register", "Register a new service (--name, --command, --credentials KEY1,KEY2, --pre-stop, --post-stop, --drain-timeout, --stop-timeout, --checkpoint-cmd)"),
        ]),
        ("watch", "Event watcher — inotify-based file watching, multi-source aggregation, event history", vec![
            ("file", "Watch a file for creation, modification, or deletion (inotify on Linux, polling fallback)"),
            ("dir", "Watch a directory for any file changes (inotify on Linux, polling fallback)"),
            ("proc", "Watch a process session for exit (--timeout N)"),
            ("on", "Subscribe to OS events: proc.exit, fs.change, service.health-fail, checkpoint.created, quota.exceeded, ipc.message, credential.expired"),
            ("multi", "Watch multiple sources simultaneously — files, dirs, procs, services (returns on first event)"),
            ("history", "View past watch events (--limit N, --since TIMESTAMP, --source TYPE)"),
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
        ("credential", "Encrypted credential store — secure secret storage with tier-based access, namespaces, TTL, auto-refresh, and bundles", vec![
            ("store", "Store a credential (--tier N, --namespace NS, --ttl SECS, --refresh-cmd CMD)"),
            ("load", "Load a credential value (tier check + expiry enforced, auto-refresh if configured)"),
            ("revoke", "Delete a stored credential"),
            ("list", "List credentials, optionally filtered by --namespace"),
            ("bundle", "Create a credential bundle (--keys key1,key2,key3)"),
            ("load-bundle", "Load all credentials in a bundle as a JSON object"),
            ("oauth-refresh", "Refresh OAuth token (google or microsoft) using stored refresh token"),
        ]),
        ("netfilter", "Outbound network firewall — domain, method, path, and binary-level rules with rate limiting", vec![
            ("add", "Add a rule (--allow|--deny <domain> [--port N] [--method GET,POST] [--path /api/**] [--binary /usr/bin/git] [--tls])"),
            ("remove", "Remove rules for a domain"),
            ("list", "List all rules and default policy"),
            ("check", "Check if a request is allowed (--method M --path P --binary B)"),
            ("reset", "Remove all rules and reset to allow-all"),
            ("default", "Set default policy (allow-all or deny-all)"),
            ("export", "Export full ruleset as JSON for proxy consumption"),
            ("rate-limit", "Set rate limit for a domain (--rpm N, --burst N)"),
            ("rate-limits", "List all rate limits"),
            ("rate-limit-remove", "Remove a rate limit for a domain"),
            ("rate-check", "Check if a request is within rate limits (records the request unless --dry-run)"),
        ]),
        ("policy", "Permission system — tier/scope checks, temporary elevation", vec![
            ("elevate", "Temporarily elevate session tier (--to N --duration SECS --reason TEXT)"),
            ("drop", "Drop an active elevation"),
            ("status", "Show current session tier, elevation, and allowed operations"),
            ("check", "Check if a specific operation (read/write/exec/net/system) is allowed"),
        ]),
        ("cron", "Agent-native job scheduler — cron with execution context, result capture, and overlap protection", vec![
            ("add", "Register a cron job (--schedule, --command, --tier, --scope, --credentials, --overlap, --timeout)"),
            ("remove", "Remove a cron job by ID"),
            ("list", "List all cron jobs with status and next run time"),
            ("status", "Detailed status of a specific job"),
            ("enable", "Enable a disabled job"),
            ("disable", "Disable a job without removing it"),
            ("logs", "View execution history for a job (--limit N)"),
            ("run", "Manually trigger a job immediately"),
            ("tick", "Process all due jobs (called by scheduler every minute)"),
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
            "try": ["cos app exec run 'ls -la <path>'", "cos app exec run 'chmod +rw <path>'"],
        }));
    }
    if err_lower.contains("no such file")
        || err_lower.contains("enoent")
        || err_lower.contains("not found")
    {
        return Some(json!({
            "hint": "File or command not found. Verify the path exists.",
            "try": ["cos app fs ls <parent-directory>", "cos app exec which <command>"],
        }));
    }
    if err_lower.contains("no space left") || err_lower.contains("enospc") {
        return Some(json!({
            "hint": "Disk full. Free space before retrying.",
            "try": ["cos sys resources", "cos app exec run 'du -sh /den/* | sort -rh | head'"],
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
            "try": ["cos proc list", "cos app exec run 'lsof -i :<port>'"],
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

/// Map an error message to a standard error code by inspecting well-known
/// substrings.  Returns `None` when the message doesn't match any pattern.
fn error_code_from_hint(error: &str) -> Option<&'static str> {
    let err_lower = error.to_lowercase();
    if err_lower.contains("permission denied") || err_lower.contains("eperm") {
        Some(crate::errors::IO_PERMISSION_DENIED)
    } else if err_lower.contains("no such file")
        || err_lower.contains("not found")
        || err_lower.contains("enoent")
    {
        Some(crate::errors::IO_FILE_NOT_FOUND)
    } else if err_lower.contains("no space left") || err_lower.contains("enospc") {
        Some(crate::errors::IO_DISK_FULL)
    } else if err_lower.contains("connection refused") || err_lower.contains("econnrefused") {
        Some(crate::errors::IO_CONNECTION_REFUSED)
    } else if err_lower.contains("timed out") || err_lower.contains("timeout") {
        Some(crate::errors::LIMIT_TIMEOUT)
    } else if err_lower.contains("already in use") || err_lower.contains("eaddrinuse") {
        Some(crate::errors::RESOURCE_BUSY)
    } else if err_lower.contains("out of memory")
        || err_lower.contains("enomem")
        || err_lower.contains("oom")
    {
        Some(crate::errors::LIMIT_OOM)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// --schema support: structured parameter introspection for every command
// ---------------------------------------------------------------------------

struct CommandSchema {
    command: &'static str,
    description: &'static str,
    params: Vec<ParamSchema>,
    example: &'static str,
}

struct ParamSchema {
    name: &'static str,
    param_type: &'static str,
    required: bool,
    description: &'static str,
    kind: &'static str, // "positional" or "flag"
}

struct Param;
impl Param {
    fn positional(
        name: &'static str,
        param_type: &'static str,
        required: bool,
        description: &'static str,
    ) -> ParamSchema {
        ParamSchema {
            name,
            param_type,
            required,
            description,
            kind: "positional",
        }
    }
    fn flag(
        name: &'static str,
        param_type: &'static str,
        required: bool,
        description: &'static str,
    ) -> ParamSchema {
        ParamSchema {
            name,
            param_type,
            required,
            description,
            kind: "flag",
        }
    }
}

fn command_schemas() -> Vec<(&'static str, &'static str, Vec<CommandSchema>)> {
    vec![
        (
            "checkpoint",
            "OverlayFS snapshot system",
            vec![
                CommandSchema {
                    command: "create",
                    description: "Freeze current changes into a named checkpoint",
                    params: vec![Param::positional(
                        "description",
                        "string",
                        true,
                        "Checkpoint description",
                    )],
                    example: "cos checkpoint create \"before refactoring\"",
                },
                CommandSchema {
                    command: "diff",
                    description: "Show created, modified, and deleted files",
                    params: vec![],
                    example: "cos checkpoint diff",
                },
                CommandSchema {
                    command: "rollback",
                    description: "Restore a checkpoint or reset to base",
                    params: vec![Param::positional(
                        "checkpoint_id",
                        "string",
                        false,
                        "Checkpoint ID to restore (omit for base)",
                    )],
                    example: "cos checkpoint rollback 002",
                },
                CommandSchema {
                    command: "list",
                    description: "List all saved checkpoints",
                    params: vec![],
                    example: "cos checkpoint list",
                },
                CommandSchema {
                    command: "status",
                    description: "Show overlay mount state and disk usage",
                    params: vec![],
                    example: "cos checkpoint status",
                },
                CommandSchema {
                    command: "quota-set",
                    description: "Set filesystem quota for the upper layer",
                    params: vec![Param::positional(
                        "size",
                        "string",
                        true,
                        "Size limit (e.g., 2G, 512M)",
                    )],
                    example: "cos checkpoint quota-set 2G",
                },
                CommandSchema {
                    command: "quota-status",
                    description: "Show current quota usage",
                    params: vec![],
                    example: "cos checkpoint quota-status",
                },
            ],
        ),
        (
            "proc",
            "Process session manager",
            vec![
                CommandSchema {
                    command: "spawn",
                    description: "Start a process in a tracked session",
                    params: vec![
                        Param::flag("--session", "string", false, "Custom session ID"),
                        Param::flag("--group", "string", false, "Named group for bulk ops"),
                        Param::flag("--tier", "integer", false, "Permission tier 0-3"),
                        Param::flag("--scope", "string", false, "Path restriction"),
                        Param::flag(
                            "--priority",
                            "enum:low|normal|high|realtime",
                            false,
                            "Process priority",
                        ),
                        Param::positional(
                            "command",
                            "string[]",
                            true,
                            "Command to run (after --)",
                        ),
                    ],
                    example: "cos proc spawn --session build-1 --group ci --tier 1 -- cargo build",
                },
                CommandSchema {
                    command: "status",
                    description: "Check if a session is running",
                    params: vec![Param::positional(
                        "session_id",
                        "string",
                        true,
                        "Session ID",
                    )],
                    example: "cos proc status build-1",
                },
                CommandSchema {
                    command: "output",
                    description: "Read buffered stdout/stderr",
                    params: vec![
                        Param::positional("session_id", "string", true, "Session ID"),
                        Param::flag("--tail", "integer", false, "Last N lines"),
                        Param::flag("--follow", "boolean", false, "Block until exit"),
                    ],
                    example: "cos proc output build-1 --tail 50",
                },
                CommandSchema {
                    command: "kill",
                    description: "Terminate a session or group",
                    params: vec![
                        Param::positional("session_id", "string", false, "Session ID"),
                        Param::flag("--group", "string", false, "Kill entire group"),
                    ],
                    example: "cos proc kill build-1",
                },
                CommandSchema {
                    command: "list",
                    description: "List all sessions",
                    params: vec![Param::flag("--group", "string", false, "Filter by group")],
                    example: "cos proc list",
                },
                CommandSchema {
                    command: "wait",
                    description: "Block until process exits",
                    params: vec![
                        Param::positional("session_id", "string", false, "Session ID"),
                        Param::flag("--group", "string", false, "Wait for group"),
                        Param::flag("--timeout", "integer", false, "Timeout in seconds"),
                    ],
                    example: "cos proc wait build-1 --timeout 300",
                },
                CommandSchema {
                    command: "result",
                    description: "Get comprehensive exit report",
                    params: vec![Param::positional(
                        "session_id",
                        "string",
                        true,
                        "Session ID",
                    )],
                    example: "cos proc result build-1",
                },
            ],
        ),
        (
            "credential",
            "Encrypted credential store",
            vec![
                CommandSchema {
                    command: "store",
                    description: "Store an encrypted credential",
                    params: vec![
                        Param::positional("name", "string", true, "Credential name"),
                        Param::positional("value", "string", true, "Secret value"),
                        Param::flag(
                            "--tier",
                            "integer",
                            false,
                            "Min tier to read (0-3, default 0)",
                        ),
                        Param::flag(
                            "--namespace",
                            "string",
                            false,
                            "Namespace (default: default)",
                        ),
                        Param::flag("--ttl", "integer", false, "Time-to-live in seconds"),
                        Param::flag(
                            "--refresh-cmd",
                            "string",
                            false,
                            "Command to execute on expiry to refresh the value",
                        ),
                    ],
                    example: "cos credential store OPENAI_KEY sk-abc123 --tier 0 --ttl 3600",
                },
                CommandSchema {
                    command: "load",
                    description: "Load a credential (tier + expiry enforced)",
                    params: vec![
                        Param::positional("name", "string", true, "Credential name"),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential load OPENAI_KEY",
                },
                CommandSchema {
                    command: "list",
                    description: "List credentials (names only, never values)",
                    params: vec![Param::flag(
                        "--namespace",
                        "string",
                        false,
                        "Filter by namespace",
                    )],
                    example: "cos credential list",
                },
                CommandSchema {
                    command: "revoke",
                    description: "Delete a credential",
                    params: vec![
                        Param::positional("name", "string", true, "Credential name"),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential revoke OPENAI_KEY",
                },
                CommandSchema {
                    command: "bundle",
                    description: "Create a credential bundle (group of keys)",
                    params: vec![
                        Param::positional("bundle_name", "string", true, "Bundle name"),
                        Param::flag(
                            "--keys",
                            "string",
                            true,
                            "Comma-separated credential names",
                        ),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential bundle openai-config --keys OPENAI_KEY,OPENAI_ORG",
                },
                CommandSchema {
                    command: "load-bundle",
                    description: "Load all credentials in a bundle",
                    params: vec![
                        Param::positional("bundle_name", "string", true, "Bundle name"),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential load-bundle openai-config",
                },
                CommandSchema {
                    command: "oauth-refresh",
                    description: "Refresh OAuth token using stored refresh token",
                    params: vec![
                        Param::positional(
                            "provider",
                            "string",
                            true,
                            "OAuth provider (google or microsoft)",
                        ),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential oauth-refresh google",
                },
            ],
        ),
        (
            "ipc",
            "Inter-process communication",
            vec![
                CommandSchema {
                    command: "send",
                    description: "Queue a message to a session",
                    params: vec![
                        Param::positional("target", "string", true, "Target session ID"),
                        Param::positional("message", "string", true, "Message body"),
                        Param::flag("--from", "string", false, "Sender session ID"),
                    ],
                    example:
                        "cos ipc send worker-1 \"task complete\" --from orchestrator",
                },
                CommandSchema {
                    command: "recv",
                    description: "Dequeue oldest message",
                    params: vec![
                        Param::positional("session_id", "string", true, "Your session ID"),
                        Param::flag("--timeout", "integer", false, "Wait timeout in seconds"),
                        Param::flag("--peek", "boolean", false, "Read without removing"),
                    ],
                    example: "cos ipc recv my-session --timeout 30",
                },
                CommandSchema {
                    command: "pipe",
                    description: "Streaming named pipes (create, publish, subscribe, list, destroy)",
                    params: vec![Param::positional(
                        "subcommand",
                        "enum:create|publish|subscribe|list|destroy",
                        true,
                        "Pipe operation",
                    )],
                    example: "cos ipc pipe create my-events --buffer-size 500",
                },
                CommandSchema {
                    command: "lock",
                    description: "Acquire a named mutex",
                    params: vec![
                        Param::positional("resource", "string", true, "Resource name"),
                        Param::flag("--holder", "string", false, "Holder session ID"),
                        Param::flag("--timeout", "integer", false, "Wait timeout"),
                    ],
                    example: "cos ipc lock database --holder agent-1 --timeout 10",
                },
            ],
        ),
        (
            "cron",
            "Agent-native job scheduler",
            vec![
                CommandSchema {
                    command: "add",
                    description: "Register a cron job",
                    params: vec![
                        Param::positional("id", "string", true, "Job ID"),
                        Param::flag("--schedule", "string", true, "Cron expression (5 fields)"),
                        Param::flag("--command", "string", true, "Command to run"),
                        Param::flag("--tier", "integer", false, "Execution tier"),
                        Param::flag("--scope", "string", false, "Path restriction"),
                        Param::flag(
                            "--credentials",
                            "string",
                            false,
                            "Comma-separated credential names",
                        ),
                        Param::flag(
                            "--overlap",
                            "enum:skip|queue|kill|allow",
                            false,
                            "Overlap policy (default: skip)",
                        ),
                        Param::flag("--timeout", "integer", false, "Kill after N seconds"),
                    ],
                    example: "cos cron add health-check --schedule \"*/5 * * * *\" --command \"cos service health my-api\" --overlap skip",
                },
                CommandSchema {
                    command: "list",
                    description: "List all cron jobs",
                    params: vec![],
                    example: "cos cron list",
                },
                CommandSchema {
                    command: "run",
                    description: "Manually trigger a job",
                    params: vec![Param::positional("id", "string", true, "Job ID")],
                    example: "cos cron run health-check",
                },
                CommandSchema {
                    command: "tick",
                    description: "Process all due jobs (called by scheduler)",
                    params: vec![],
                    example: "cos cron tick",
                },
            ],
        ),
        (
            "service",
            "Service lifecycle manager",
            vec![
                CommandSchema {
                    command: "start",
                    description: "Start a service (pre_start → credential injection → spawn → health → post_start)",
                    params: vec![Param::positional("name", "string", true, "Service name")],
                    example: "cos service start my-api",
                },
                CommandSchema {
                    command: "stop",
                    description: "Graceful stop (checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop)",
                    params: vec![Param::positional("name", "string", true, "Service name")],
                    example: "cos service stop my-api",
                },
                CommandSchema {
                    command: "stop-all",
                    description: "Stop all services in reverse dependency order",
                    params: vec![],
                    example: "cos service stop-all",
                },
                CommandSchema {
                    command: "register",
                    description: "Register a new service",
                    params: vec![
                        Param::flag("--name", "string", true, "Service name"),
                        Param::flag("--command", "string", true, "Start command"),
                        Param::flag("--workdir", "string", false, "Working directory"),
                        Param::flag("--health-url", "string", false, "Health check URL"),
                        Param::flag(
                            "--credentials",
                            "string",
                            false,
                            "Credential names (comma-separated)",
                        ),
                        Param::flag("--pre-start", "string", false, "Pre-start hook command"),
                        Param::flag("--pre-stop", "string", false, "Pre-stop hook command"),
                        Param::flag("--post-stop", "string", false, "Post-stop hook command"),
                        Param::flag("--drain-timeout", "integer", false, "Drain wait seconds"),
                        Param::flag(
                            "--stop-timeout",
                            "integer",
                            false,
                            "SIGTERM→SIGKILL seconds",
                        ),
                        Param::flag(
                            "--checkpoint-cmd",
                            "string",
                            false,
                            "State checkpoint command",
                        ),
                    ],
                    example: "cos service register --name my-api --command \"python app.py\" --health-url http://localhost:8000/health --credentials OPENAI_KEY,DB_URL",
                },
            ],
        ),
        (
            "watch",
            "Event-driven watcher",
            vec![
                CommandSchema {
                    command: "file",
                    description: "Watch a file for changes (inotify on Linux)",
                    params: vec![
                        Param::positional("path", "string", true, "File path"),
                        Param::flag("--timeout", "integer", false, "Timeout in seconds"),
                    ],
                    example: "cos watch file /den/config.json --timeout 30",
                },
                CommandSchema {
                    command: "multi",
                    description: "Watch multiple sources simultaneously",
                    params: vec![
                        Param::flag("--file", "string", false, "File to watch (repeatable)"),
                        Param::flag(
                            "--dir",
                            "string",
                            false,
                            "Directory to watch (repeatable)",
                        ),
                        Param::flag(
                            "--proc",
                            "string",
                            false,
                            "Process session to watch (repeatable)",
                        ),
                        Param::flag(
                            "--service",
                            "string",
                            false,
                            "Service to watch (repeatable)",
                        ),
                        Param::flag("--timeout", "integer", false, "Timeout in seconds"),
                    ],
                    example: "cos watch multi --file /den/main.py --proc worker-1 --service my-api --timeout 60",
                },
                CommandSchema {
                    command: "history",
                    description: "View past watch events",
                    params: vec![
                        Param::flag("--limit", "integer", false, "Max events (default 50)"),
                        Param::flag("--since", "string", false, "ISO timestamp filter"),
                        Param::flag(
                            "--source",
                            "enum:file|dir|proc|service",
                            false,
                            "Source type filter",
                        ),
                    ],
                    example: "cos watch history --limit 20 --source file",
                },
            ],
        ),
        (
            "netfilter",
            "Network firewall with rate limiting",
            vec![
                CommandSchema {
                    command: "add",
                    description: "Add allow/deny rule",
                    params: vec![
                        Param::flag("--allow", "string", false, "Domain to allow"),
                        Param::flag("--deny", "string", false, "Domain to deny"),
                        Param::flag("--port", "integer", false, "Port number"),
                    ],
                    example: "cos netfilter add --allow api.openai.com --port 443",
                },
                CommandSchema {
                    command: "rate-limit",
                    description: "Set rate limit for a domain",
                    params: vec![
                        Param::positional("domain", "string", true, "Domain"),
                        Param::flag("--rpm", "integer", true, "Requests per minute"),
                        Param::flag("--burst", "integer", false, "Burst allowance"),
                    ],
                    example: "cos netfilter rate-limit api.openai.com --rpm 60 --burst 10",
                },
                CommandSchema {
                    command: "rate-check",
                    description: "Check/record a request against rate limits",
                    params: vec![
                        Param::positional("domain", "string", true, "Domain to check"),
                        Param::flag(
                            "--dry-run",
                            "boolean",
                            false,
                            "Check without recording",
                        ),
                    ],
                    example: "cos netfilter rate-check api.openai.com",
                },
            ],
        ),
        (
            "sandbox",
            "Process isolation",
            vec![CommandSchema {
                command: "exec",
                description: "Run in isolated namespace + cgroup",
                params: vec![
                    Param::flag("--mem", "string", false, "Memory limit (e.g., 512M)"),
                    Param::flag("--cpu", "integer", false, "CPU percent"),
                    Param::flag("--pids", "integer", false, "Max processes"),
                    Param::flag("--timeout", "integer", false, "Kill after N seconds"),
                    Param::flag("--no-network", "boolean", false, "Disable network"),
                    Param::flag(
                        "--seccomp-profile",
                        "enum:minimal|network|full",
                        false,
                        "Syscall filter",
                    ),
                    Param::positional("command", "string[]", true, "Command (after --)"),
                ],
                example:
                    "cos sandbox exec --no-network --mem 256M --timeout 30 -- python untrusted.py",
            }],
        ),
        (
            "policy",
            "Permission system",
            vec![
                CommandSchema {
                    command: "status",
                    description: "Show current tier and allowed operations",
                    params: vec![],
                    example: "cos policy status",
                },
                CommandSchema {
                    command: "check",
                    description: "Test if an operation is allowed",
                    params: vec![Param::positional(
                        "operation",
                        "enum:read|write|delete|exec|net|system",
                        true,
                        "Operation to check",
                    )],
                    example: "cos policy check exec",
                },
                CommandSchema {
                    command: "elevate",
                    description: "Temporarily escalate privileges",
                    params: vec![
                        Param::flag("--to", "integer", true, "Target tier (0-3)"),
                        Param::flag("--duration", "integer", true, "Seconds"),
                        Param::flag("--reason", "string", true, "Reason for elevation"),
                    ],
                    example: "cos policy elevate --to 1 --duration 300 --reason \"deployment\"",
                },
            ],
        ),
        (
            "sys",
            "System information",
            vec![
                CommandSchema {
                    command: "info",
                    description: "OS, architecture, hostname, version",
                    params: vec![],
                    example: "cos sys info",
                },
                CommandSchema {
                    command: "resources",
                    description: "Disk, memory, CPU usage",
                    params: vec![],
                    example: "cos sys resources",
                },
                CommandSchema {
                    command: "env",
                    description: "Environment variables",
                    params: vec![Param::positional(
                        "pattern",
                        "string",
                        false,
                        "Filter pattern",
                    )],
                    example: "cos sys env COS",
                },
                CommandSchema {
                    command: "proc",
                    description: "All processes with resource usage",
                    params: vec![],
                    example: "cos sys proc",
                },
            ],
        ),
    ]
}

fn show_command_schema(app_name: &str, command: &str) -> Result<Option<String>, String> {
    let schemas = command_schemas();
    let app = schemas.iter().find(|(n, _, _)| *n == app_name);
    let app = app.ok_or_else(|| format!("no schema for: {app_name}"))?;

    let cmd = app.2.iter().find(|c| c.command == command);
    let cmd = cmd.ok_or_else(|| format!("no schema for: {app_name} {command}"))?;

    let params: Vec<Value> = cmd
        .params
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "type": p.param_type,
                "required": p.required,
                "description": p.description,
                "kind": p.kind,
            })
        })
        .collect();

    let output = json!({
        "command": format!("cos {app_name} {}", cmd.command),
        "description": cmd.description,
        "parameters": params,
        "example": cmd.example,
    });
    Ok(Some(output.to_string()))
}

fn show_builtin_schema(app_name: &str) -> Result<Option<String>, String> {
    let schemas = command_schemas();
    let app = schemas.iter().find(|(n, _, _)| *n == app_name);
    let app = app.ok_or_else(|| format!("no schema for: {app_name}"))?;

    let commands: Vec<Value> = app
        .2
        .iter()
        .map(|cmd| {
            let params: Vec<Value> = cmd
                .params
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name,
                        "type": p.param_type,
                        "required": p.required,
                        "description": p.description,
                        "kind": p.kind,
                    })
                })
                .collect();
            json!({
                "command": cmd.command,
                "description": cmd.description,
                "parameters": params,
                "example": cmd.example,
            })
        })
        .collect();

    let output = json!({
        "app": app_name,
        "description": app.1,
        "commands": commands,
    });
    Ok(Some(output.to_string()))
}

fn show_app_command_schema(
    app_name: &str,
    command: &str,
    app: &apps::App,
) -> Result<Option<String>, String> {
    let desc = app
        .manifest
        .commands
        .get(command)
        .map(|s| s.as_str())
        .unwrap_or("No description");

    let output = json!({
        "command": format!("cos app {app_name} {command}"),
        "description": desc,
        "hint": format!("Run: cos app {app_name} {command} --help for usage details"),
    });
    Ok(Some(output.to_string()))
}

fn show_app_schema(app_name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let commands: serde_json::Map<String, Value> = app
        .manifest
        .commands
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();

    let output = json!({
        "app": app_name,
        "description": app.manifest.description,
        "commands": commands,
        "hint": format!("Run: cos app {app_name} <command> --schema for command details"),
    });
    Ok(Some(output.to_string()))
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

    // cos <primitive> --schema → show all command schemas for this primitive
    if args.len() == 2 && args[1] == "--schema" {
        return show_builtin_schema(app_name);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();

    // If --schema is in args, return schema instead of executing
    if cmd_args.contains(&"--schema".to_string()) {
        return show_command_schema(app_name, command);
    }

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
            .any(|v| v.as_str().unwrap().contains("cos app fs ls")));
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

    #[test]
    fn schema_for_known_builtin() {
        let schemas = command_schemas();
        assert!(schemas.iter().any(|(n, _, _)| *n == "checkpoint"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "proc"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "credential"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "cron"));
    }

    #[test]
    fn show_command_schema_returns_json() {
        let result = show_command_schema("checkpoint", "create");
        assert!(result.is_ok());
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["command"], "cos checkpoint create");
        assert!(v["parameters"].is_array());
        assert!(v["example"].is_string());
    }

    #[test]
    fn show_builtin_schema_returns_all_commands() {
        let result = show_builtin_schema("proc");
        assert!(result.is_ok());
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        assert!(v["commands"].is_array());
        assert!(v["commands"].as_array().unwrap().len() > 3);
    }

    #[test]
    fn show_command_schema_unknown_returns_error() {
        let result = show_command_schema("nonexistent", "cmd");
        assert!(result.is_err());
    }

    #[test]
    fn show_command_schema_unknown_command_returns_error() {
        let result = show_command_schema("checkpoint", "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn show_command_schema_has_param_details() {
        let result = show_command_schema("proc", "spawn");
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        let params = v["parameters"].as_array().unwrap();
        assert!(!params.is_empty());
        // Each param should have name, type, required, description, kind
        for p in params {
            assert!(p["name"].is_string());
            assert!(p["type"].is_string());
            assert!(p["required"].is_boolean());
            assert!(p["description"].is_string());
            assert!(
                p["kind"] == "positional" || p["kind"] == "flag",
                "kind must be positional or flag, got: {}",
                p["kind"]
            );
        }
    }

    #[test]
    fn show_builtin_schema_all_primitives() {
        // Every primitive that has a schema should produce valid output
        let primitives = [
            "checkpoint",
            "proc",
            "credential",
            "ipc",
            "cron",
            "service",
            "watch",
            "netfilter",
            "sandbox",
            "policy",
            "sys",
        ];
        for name in &primitives {
            let result = show_builtin_schema(name);
            assert!(result.is_ok(), "Failed for primitive: {name}");
            let output = result.unwrap().unwrap();
            let v: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(v["app"], *name);
            assert!(v["description"].is_string());
            assert!(v["commands"].is_array());
            assert!(
                !v["commands"].as_array().unwrap().is_empty(),
                "No commands for: {name}"
            );
        }
    }

    #[test]
    fn error_code_from_hint_maps_correctly() {
        assert_eq!(
            error_code_from_hint("Permission denied on /etc"),
            Some(crate::errors::IO_PERMISSION_DENIED)
        );
        assert_eq!(
            error_code_from_hint("No such file: /missing"),
            Some(crate::errors::IO_FILE_NOT_FOUND)
        );
        assert_eq!(
            error_code_from_hint("connection refused"),
            Some(crate::errors::IO_CONNECTION_REFUSED)
        );
        assert_eq!(
            error_code_from_hint("No space left on device"),
            Some(crate::errors::IO_DISK_FULL)
        );
        assert_eq!(
            error_code_from_hint("Operation timed out"),
            Some(crate::errors::LIMIT_TIMEOUT)
        );
        assert_eq!(
            error_code_from_hint("address already in use"),
            Some(crate::errors::RESOURCE_BUSY)
        );
        assert_eq!(
            error_code_from_hint("out of memory"),
            Some(crate::errors::LIMIT_OOM)
        );
        assert_eq!(error_code_from_hint("something random"), None);
    }
}

use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::agent;
use crate::apps;
use crate::audit;
use crate::bridge;
use crate::checkpoint;
use crate::credential;
use crate::cron;
use crate::engine_pkg;
use crate::model;
use crate::caps;
use crate::perms;
use crate::service;
use crate::sysinfo;

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

    // Top-level help / version flags. Match what every Unix CLI does so
    // muscle memory works: bare `cos --help` / `cos help` is the same
    // overview as bare `cos`; `cos help <topic>` drills into one
    // primitive/app; `cos --version` prints just the version envelope.
    match name.as_str() {
        "--help" | "-h" => {
            if args.len() >= 2 {
                return show_help_for(&args[1]);
            }
            return show_overview();
        }
        "help" => {
            if args.len() >= 2 {
                return show_help_for(&args[1]);
            }
            return show_overview();
        }
        "--version" | "-v" | "-V" => {
            return Ok(Some(
                json!({"name": "cos", "version": VERSION}).to_string(),
            ));
        }
        _ => {}
    }

    // "app" namespace → route to Python apps
    if name == "app" {
        return dispatch_app(&args[1..]);
    }

    // Built-in OS primitives
    match name.as_str() {
        "sys" => dispatch_builtin(args, "sys", sysinfo::run),
        "service" => dispatch_builtin(args, "service", service::run),
        "checkpoint" => dispatch_builtin(args, "checkpoint", checkpoint::run),
        "credential" => dispatch_builtin(args, "credential", credential::run),
        // `perms` is invoked by Python apps (apps/_lib/policy.py shells to
        // `cos perms check`) and not directly by users — kept dispatchable
        // but hidden from the user-facing overview list.
        "perms" => dispatch_builtin(args, "perms", perms::run),
        "cron" => dispatch_builtin(args, "cron", cron::run),
        "agent" => dispatch_agent(args),
        "model" => dispatch_builtin(args, "model", model::run),
        "engine" => dispatch_builtin(args, "engine", engine_pkg::run),
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

    // "cos app" with no further args (or with --help/help) → list apps.
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        return show_apps(&discovered);
    }

    let app_name = &args[0];

    // Special: `cos app lint [<name>]` — refuses AI-using apps that
    // import provider SDKs directly. Run before the "unknown app"
    // check so `lint` itself doesn't collide with an app name.
    if app_name == "lint" {
        let target = args.get(1).map(String::as_str);
        return lint_apps(&discovered, target);
    }

    // Check if it's a known app
    if !discovered.contains_key(app_name.as_str()) {
        let names: Vec<&String> = discovered.keys().collect();
        return Err(format!("unknown app: {app_name}. installed: {names:?}"));
    }

    // "cos app <name>" / "cos app <name> --help|-h|help" → show app help.
    if args.len() == 1 || (args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h" | "help"))
    {
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
    if !app.manifest.operations.contains_key(command.as_str()) {
        let valid: Vec<&String> = app.manifest.operations.keys().collect();
        return Err(format!(
            "unknown command: cos app {app_name} {command}. available: {valid:?}"
        ));
    }

    run_app_command(app_name, command, &cmd_args, app)
}

/// `cos app lint [<name>]` — refuse apps that smuggle in AI SDKs.
///
/// Apps are required to route every model call through the kernel's
/// `cos agent chat --app <id>` gate (via `apps/_lib/ai.py`). Importing
/// `openai`, `anthropic`, or `google.generativeai` directly would
/// bypass budget, safety, and audit — so the linter looks for those
/// imports in every `*.py` file under each app's directory and reports
/// the offenders.
fn lint_apps(
    discovered: &std::collections::BTreeMap<String, apps::App>,
    target: Option<&str>,
) -> Result<Option<String>, String> {
    let mut results = Vec::new();
    let mut any_violation = false;

    let apps_to_check: Vec<&apps::App> = match target {
        Some(name) => match discovered.get(name) {
            Some(a) => vec![a],
            None => {
                let names: Vec<&String> = discovered.keys().collect();
                return Err(format!("unknown app: {name}. installed: {names:?}"));
            }
        },
        None => discovered.values().collect(),
    };

    for app in apps_to_check {
        let violations = scan_app_for_ai_imports(&app.dir);
        if !violations.is_empty() {
            any_violation = true;
        }
        results.push(json!({
            "app": app.manifest.id,
            "ok": violations.is_empty(),
            "violations": violations,
        }));
    }

    Ok(Some(
        json!({
            "results": results,
            "ok": !any_violation,
            "hint": if any_violation {
                "Apps must import from `_lib.ai` (which shells out to `cos agent chat --app <id>`); \
                 they must not import provider SDKs directly."
            } else {
                "All apps route their AI calls through the kernel gate."
            },
        })
        .to_string(),
    ))
}

/// Walk an app directory looking for `*.py` files that import one of
/// the forbidden provider SDKs. Returns a list of `{file, line, text}`
/// hits.
fn scan_app_for_ai_imports(app_dir: &Path) -> Vec<Value> {
    const FORBIDDEN: &[&str] = &[
        "openai",
        "anthropic",
        "google.generativeai",
        "vertexai",
        "cohere",
        "mistralai",
        "replicate",
        "boto3.client(\"bedrock",
        "boto3.client('bedrock",
    ];
    let mut hits = Vec::new();
    walk_py(app_dir, &mut |path, contents| {
        for (idx, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("import ") || trimmed.starts_with("from ")) {
                // Allow grepping for the boto3-bedrock shape too.
                if !FORBIDDEN.iter().any(|f| trimmed.contains(f)) {
                    continue;
                }
            }
            for needle in FORBIDDEN {
                if trimmed.contains(needle)
                    && (trimmed.starts_with("import ")
                        || trimmed.starts_with("from ")
                        || trimmed.contains(".client"))
                {
                    hits.push(json!({
                        "file": path.display().to_string(),
                        "line": idx + 1,
                        "text": line.to_string(),
                        "matched": needle.to_string(),
                    }));
                    break;
                }
            }
        }
    });
    hits
}

fn walk_py(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // Skip vendored / build / hidden directories.
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "__pycache__" {
                continue;
            }
            walk_py(&p, f);
        } else if p.extension().and_then(|e| e.to_str()) == Some("py") {
            if let Ok(contents) = std::fs::read_to_string(&p) {
                f(&p, &contents);
            }
        }
    }
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
        "hint": "Run: cos <primitive> <command> for OS operations. cos help <primitive> for one. cos app to see available apps.",
    });
    Ok(Some(output.to_string()))
}

/// `cos help <topic>` — focused help for one primitive or app. Falls
/// back to the global overview when the topic is unknown so the user
/// always sees something useful (and the available names).
fn show_help_for(topic: &str) -> Result<Option<String>, String> {
    // Built-in primitives use the same shape as `cos <primitive>`
    // (no args).
    if let Some((name, desc, cmds)) = builtin_apps().into_iter().find(|(n, _, _)| *n == topic) {
        let cmd_map: serde_json::Map<String, Value> = cmds
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        return Ok(Some(
            json!({
                "app": name,
                "description": desc,
                "commands": cmd_map,
                "hint": format!("Run: cos {name} <command> [args]"),
            })
            .to_string(),
        ));
    }

    // Apps: render the same help as `cos app <name>`.
    let discovered = apps::discover(&apps_dir());
    if let Some(app) = discovered.get(topic) {
        return show_app_help(topic, app);
    }
    // `cos help app` → list all apps.
    if topic == "app" {
        return show_apps(&discovered);
    }

    // Unknown topic: degrade to the overview but include a note so the
    // caller knows their topic wasn't recognised.
    let mut overview: Value = match show_overview()? {
        Some(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        None => json!({}),
    };
    if let Some(obj) = overview.as_object_mut() {
        obj.insert(
            "note".into(),
            json!(format!("unknown help topic: {topic}")),
        );
    }
    Ok(Some(overview.to_string()))
}

fn show_apps(
    discovered: &std::collections::BTreeMap<String, apps::App>,
) -> Result<Option<String>, String> {
    let mut app_list = Vec::new();
    for (name, app) in discovered {
        let cmds: serde_json::Map<String, Value> = app
            .manifest
            .operations
            .iter()
            .map(|(k, op)| (k.clone(), json!(op.label.current())))
            .collect();
        app_list.push(json!({
            "name": name,
            "label": app.manifest.name.current(),
            "description": app.manifest.summary.current(),
            "commands": cmds,
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
    let cmds: serde_json::Map<String, Value> = app
        .manifest
        .operations
        .iter()
        .map(|(k, op)| (k.clone(), json!(op.label.current())))
        .collect();
    let output = json!({
        "app": name,
        "label": app.manifest.name.current(),
        "version": app.manifest.version,
        "description": app.manifest.summary.current(),
        "commands": cmds,
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

    // Capability gate: callers (interactive CLI or agent) must hold
    // `agent.invoke` on the app's name to dispatch any command.
    // Schema introspection is allowed unconditionally so tooling can
    // describe apps it cannot run. Strict is the default mode — the
    // user-terminal CLI gets its caps from the session it was started
    // in; ad-hoc development can opt into `COS_PERMS_MODE=permissive`.
    if command != "__schema__" {
        if let Err(denial) = caps::require(
            caps::Verb::AGENT_INVOKE,
            caps::Scope::name(app_name),
        ) {
            return Err(denial.summary());
        }
    }

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
        ("agent", "OS-native agent subsystem — runtime, memory, skills, LLM providers, tools, FS job queue", vec![
            ("setup", "Per-modality config wizard: cos agent setup <llm|tts|stt|imagegen|embed|all> [--status|--reset|--verify-only|--no-verify]. Bare `cos agent setup` opens an interactive modality picker."),
            ("ask", "Single-shot prompt with full tool/memory loop: cos agent ask \"<prompt>\" [--stream] — without --stream waits for the full response; with --stream tokens are written live to stderr while the JSON envelope still lands on stdout."),
            ("chat", "Two modes — (1) interactive REPL for the system agent: cos agent chat [--session <id>] [--no-stream] [--no-memory] [--show-tools] [--max-turns N] (slash commands: /quit /help /session /clear /history [N] /tools); (2) one-shot app-gated chat for installed apps: cos agent chat --app <id> [--prompt <text>] [--prompt-file <p>] [--model <name>] [--origin trusted|user-input|external-content] [--max-units N] [--system <text>] [--embed] [--image-input <p>|--image-output <p>] [--audio-input <p>|--audio-output <p>] [--video-input <p>|--video-output <p>]. Modality (chat/embed/image/audio/vision/video) is auto-derived from the request shape; verbs are never passed at the CLI. The mode is selected by whether --app is present."),
            ("budget", "Inspect or reset an app's monthly AI budget: cos agent budget show|reset|history <app>. The system agent reports under the pseudo-app id `system.agent`."),
            ("status", "Short live verdict: provider/model/key source, ready/not-ready, most-recent session. Use `cos agent doctor` for the full provider matrix, tool list, skills, usage."),
            ("sessions", "Inspect / manage conversation sessions in the memory DB: cos agent sessions [list [N] | title <id> | set-title <id> \"<title>\" | count [<id>] | clear <id> --yes]"),
            ("recall", "FTS5 search across recorded conversations: cos agent recall \"<query>\" [limit]"),
            ("service", "Filesystem-based job queue: cos agent service {submit \"<prompt>\" | list | status <id> | result <id> | work | cancel <id> | prune}. Composes with cos cron + cos service for managed background workers."),
            ("notes", "Manage agent markdown notes (MEMORY.md / USER.md / custom): cos agent notes [list|read <n>|write <n> <content>|append <n> <line>|delete <n>]"),
            ("skills", "Inspect or install skill bundles: cos agent skills [list|info <id>|install <archive.zip>|hub <list|show|install> <owner/repo>|...]"),
            ("todo", "Manage per-session agent todo lists: cos agent todo [list <session_id>|add <session_id> <id> <title>|set-status ...|remove ...|clear ...]"),
            ("mcp", "MCP (Model Context Protocol) bridge — server exposes the cos agent tool catalogue; client probes/invokes a remote MCP subprocess"),
            ("doctor", "Aggregate diagnostic — provider config matrix, engines, memory, skills, hooks, audit/run-log + last 7d usage & insights. Add --probe-network for a live provider ping."),
            ("dev", "Power-user / internal namespace — exposes building blocks (token estimator, redactor, scrubbers, classifier, diagnostics dumps). Run `cos agent dev` for the list. Not a stable surface."),
        ]),
        ("model", "Local model registry + inference daemon (ort for STT/TTS/embed/vision/imagegen, llama.cpp for LLM)", vec![
            ("list", "List registered models from /var/lib/cos/models/"),
            ("import", "Register a local ONNX/GGUF file: cos model import <path> --as <name> [--version <v>] [--task llm|stt|tts|embed|vision|imagegen] [--engine ort|llama] [--format onnx|gguf] [--device <id>] [--move] [--force]"),
            ("rm", "Remove a registered model: cos model rm <name>@<version>"),
            ("check", "Check engine compatibility for a model: cos model check <name>@<version>"),
            ("load", "Load a registered model into the runtime daemon"),
            ("unload", "Unload a model from the runtime"),
            ("infer", "Run inference (routed via IPC to model-runtime daemon)"),
            ("status", "Runtime status — loaded models, RAM, devices, linked engines"),
            ("bench", "Benchmark a model"),
        ]),
        ("engine", "Native inference engine package manager — install / activate / rollback llama.cpp, ort, ort-genai versions side-by-side", vec![
            ("list", "List installed engines and their active versions"),
            ("info", "Detailed info for one engine: cos engine info <name>"),
            ("install", "Install from a local archive: cos engine install <name>@<version> --from <path.zip> [--no-activate]"),
            ("activate", "Switch active version: cos engine activate <name>@<version>"),
            ("rollback", "Swap active <-> previous: cos engine rollback <name>"),
            ("update", "Fetch + install from GitHub Releases: cos engine update <name> [--check] [--to <tag>] [--force] [--accelerator cpu|cuda|vulkan|...] [--no-activate]"),
            ("pin", "Lock active version against auto-update: cos engine pin <name>[@<version>]"),
            ("unpin", "Remove pin: cos engine unpin <name>"),
            ("gc", "Delete old installed versions, keep last N (default 3): cos engine gc <name> [--keep N]"),
            ("uninstall", "Remove a specific installed version: cos engine uninstall <name>@<version>"),
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
            "try": ["cos sys resources", "cos app exec run 'du -sh $HOME/* | sort -rh | head'"],
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
    // Call the Python app with __schema__ to get live schema
    let data_dir = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    match bridge::run_python_app(&app.dir, "__schema__", &[], &data_dir, &apps) {
        Ok(Some(output)) => {
            if let Ok(schema) = serde_json::from_str::<Value>(&output) {
                if let Some(cmd_schema) = schema.get(command) {
                    let desc = app
                        .manifest
                        .operations
                        .get(command)
                        .map(|op| op.summary.current().to_string())
                        .unwrap_or_else(|| "No description".to_string());
                    let mut result = json!({
                        "command": format!("cos app {app_name} {command}"),
                        "description": desc,
                    });
                    if let Some(params) = cmd_schema.get("parameters") {
                        result["parameters"] = params.clone();
                    }
                    if let Some(example) = cmd_schema.get("example") {
                        result["example"] = example.clone();
                    }
                    return Ok(Some(result.to_string()));
                }
            }
            // Schema returned but command not found in it
            let desc = app
                .manifest
                .operations
                .get(command)
                .map(|op| op.summary.current().to_string())
                .unwrap_or_else(|| "No description".to_string());
            Ok(Some(
                json!({
                    "command": format!("cos app {app_name} {command}"),
                    "description": desc,
                })
                .to_string(),
            ))
        }
        _ => {
            // App doesn't support __schema__ — return basic info
            let desc = app
                .manifest
                .operations
                .get(command)
                .map(|op| op.summary.current().to_string())
                .unwrap_or_else(|| "No description".to_string());
            Ok(Some(
                json!({
                    "command": format!("cos app {app_name} {command}"),
                    "description": desc,
                })
                .to_string(),
            ))
        }
    }
}

fn show_app_schema(app_name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let data_dir = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    // Try to get live schema from the app
    let live_schema = match bridge::run_python_app(&app.dir, "__schema__", &[], &data_dir, &apps) {
        Ok(Some(output)) => serde_json::from_str::<Value>(&output).ok(),
        _ => None,
    };

    let mut commands = Vec::new();
    for (cmd_name, op) in &app.manifest.operations {
        let mut entry = json!({
            "command": cmd_name,
            "label": op.label.current(),
            "description": op.summary.current(),
        });
        if let Some(ref schema) = live_schema {
            if let Some(cmd_schema) = schema.get(cmd_name.as_str()) {
                if let Some(params) = cmd_schema.get("parameters") {
                    entry["parameters"] = params.clone();
                }
                if let Some(example) = cmd_schema.get("example") {
                    entry["example"] = example.clone();
                }
            }
        }
        commands.push(entry);
    }

    let output = json!({
        "app": app_name,
        "label": app.manifest.name.current(),
        "description": app.manifest.summary.current(),
        "commands": commands,
    });
    Ok(Some(output.to_string()))
}

/// Special-case dispatcher for `cos agent` that turns a bare
/// invocation (no subcommand) on an interactive TTY into either
/// `setup` (when the agent has not been configured yet) or `chat`
/// (when it has). Falls through to the standard help-table behavior
/// for non-TTY callers — scripts piping `cos agent | jq` still see
/// the machine-readable command list — and for explicit `--help`.
fn dispatch_agent(args: &[String]) -> Result<Option<String>, String> {
    // Explicit help should not be hijacked.
    let explicit_help = args.len() >= 2
        && matches!(args[1].as_str(), "--help" | "-h" | "help" | "--schema");
    if !explicit_help && args.len() == 1 {
        use std::io::IsTerminal;
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if interactive {
            let cfg = &crate::config::get().agent;
            let mut rewritten: Vec<String> = Vec::with_capacity(3);
            rewritten.push(args[0].clone());
            if agent::setup::is_ready(cfg).is_ok() {
                rewritten.push("chat".into());
            } else {
                // Land directly on the LLM wizard rather than the
                // modality picker — `cos agent` not being ready almost
                // always means the conversational LLM isn't configured.
                rewritten.push("setup".into());
                rewritten.push("llm".into());
            }
            return dispatch_builtin(&rewritten, "agent", agent::run);
        }
    }
    dispatch_builtin(args, "agent", agent::run)
}

fn dispatch_builtin(
    args: &[String],
    app_name: &str,
    handler: fn(&str, &[String]) -> Result<Value, String>,
) -> Result<Option<String>, String> {
    // `cos <primitive>` and `cos <primitive> --help|-h|help` render the
    // same machine-readable command list. Doing this here means every
    // primitive picks up help support uniformly.
    let help_only = args.len() == 1
        || (args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h" | "help"));
    if help_only {
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
        let hint = recovery_hint("Permission denied on /home/cos/file.txt").unwrap();
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
        let hint = recovery_hint("No such file or directory: /home/cos/missing").unwrap();
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
        assert!(schemas.iter().any(|(n, _, _)| *n == "credential"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "cron"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "service"));
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
        let result = show_builtin_schema("credential");
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
        let result = show_command_schema("checkpoint", "create");
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
            "credential",
            "cron",
            "service",
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

    fn parse(out: Option<String>) -> Value {
        serde_json::from_str(&out.expect("dispatch returned None")).expect("not JSON")
    }

    #[test]
    fn dispatch_help_flag_returns_overview() {
        let v = parse(dispatch(&["--help".into()]).unwrap());
        assert_eq!(v["name"], "cos");
        assert!(v["primitives"].is_array());
    }

    #[test]
    fn dispatch_h_short_flag_returns_overview() {
        let v = parse(dispatch(&["-h".into()]).unwrap());
        assert_eq!(v["name"], "cos");
    }

    #[test]
    fn dispatch_bare_help_returns_overview() {
        let v = parse(dispatch(&["help".into()]).unwrap());
        assert!(v["primitives"].is_array());
    }

    #[test]
    fn dispatch_help_topic_returns_primitive() {
        let v = parse(dispatch(&["help".into(), "sys".into()]).unwrap());
        assert_eq!(v["app"], "sys");
        assert!(v["commands"].is_object());
    }

    #[test]
    fn dispatch_help_unknown_topic_returns_overview_with_note() {
        let v = parse(dispatch(&["help".into(), "nope".into()]).unwrap());
        assert!(v["primitives"].is_array());
        assert!(v["note"].as_str().unwrap().contains("unknown help topic"));
    }

    #[test]
    fn dispatch_version_returns_envelope() {
        for flag in ["--version", "-v", "-V"] {
            let v = parse(dispatch(&[flag.into()]).unwrap());
            assert_eq!(v["name"], "cos");
            assert_eq!(v["version"], VERSION);
        }
    }

    #[test]
    fn dispatch_builtin_help_token_returns_overview() {
        for flag in ["--help", "-h", "help"] {
            let v = parse(dispatch(&["sys".into(), flag.into()]).unwrap());
            assert_eq!(v["app"], "sys", "flag: {flag}");
            assert!(v["commands"].is_object());
        }
    }

    #[test]
    fn dispatch_agent_help_does_not_hijack() {
        // `cos agent --help` must return the command list rather than
        // dropping into the interactive chat/setup shortcut.
        let v = parse(dispatch(&["agent".into(), "--help".into()]).unwrap());
        assert_eq!(v["app"], "agent");
        assert!(v["commands"].is_object());
    }

    #[test]
    fn browser_module_compiles() {
        // cos browser is no longer a user CLI primitive — it's exposed
        // only as the `cos_browser` agent tool. Smoke-test that the module
        // is still wired up by reaching the unknown-command path.
        let err = crate::browser::run("__nope__", &[]).unwrap_err();
        assert!(err.contains("unknown"));
    }
}

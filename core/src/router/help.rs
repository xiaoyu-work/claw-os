//! User-facing help catalogs and command schema rendering for the router.

use serde_json::{json, Value};

use super::{apps_dir, VERSION};
use crate::apps;

pub(super) fn show_overview() -> Result<Option<String>, String> {
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
pub(super) fn show_help_for(topic: &str) -> Result<Option<String>, String> {
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
        obj.insert("note".into(), json!(format!("unknown help topic: {topic}")));
    }
    Ok(Some(overview.to_string()))
}

pub(super) fn show_apps(
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
        "hint": "Run: cos app <name> for app details, cos app <name> <command> [args] to execute. Scaffold a new App with: cos app create <id> [--kind cli|desktop|both]. Install an App with: cos app install <source-dir>",
    });
    Ok(Some(output.to_string()))
}

pub(super) fn show_app_help(name: &str, app: &apps::App) -> Result<Option<String>, String> {
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

pub(super) fn builtin_apps() -> Vec<(
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
            ("oauth-login", "Complete Google PKCE or Microsoft device-code login"),
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
        ("ai", "App-facing AI gate — single-shot LLM / embedding / image / audio / video calls scoped to one installed App. Distinct from `cos agent`: this is the App-developer-facing primitive, not the kernel Agent product.", vec![
            ("chat", "Stable one-shot App-gated text chat: cos ai chat --app <id> [--prompt <text>|--prompt-file <p>] [--origin trusted|user-input|external-content] [--max-units N] [--system <text>|--system-file <p>]. external-content selects ai.chat.untrusted. Embed/image/vision/audio/video selectors are experimental and currently return unsupported."),
            ("tool", "Invoke one App-facing Tool by name: cos ai tool <name> --app <id> [--args <json>|--args-file <p>]. The kernel checks the App's caps grants, runs the Tool, and writes one audit row per call. List tools with `cos ai tools`."),
            ("tools", "Print the catalog of App-facing Tools (name, summary, verb, stability, JSON-Schema for args and return). Used by App authors and LLM function-call spec generators."),
        ]),
        ("agent", "OS-native agent subsystem — clawd-backed runtime, memory, skills, LLM providers, tools, and tasks", vec![
            ("setup", "Per-modality config wizard: cos agent setup <text|tts|stt|imagegen|embed|all> [--status|--reset|--verify-only|--no-verify]. Bare `cos agent setup` opens an interactive modality picker."),
            ("ask", "Single-shot prompt with full tool/memory loop: cos agent ask \"<prompt>\" [--full] [--session <id>] [--timeout-secs <n>]. Default prints just the model's plain-text answer; add --full for the JSON envelope, --session to continue an existing task conversation, or a timeout to cancel an overlong clawd task."),
            ("chat", "Interactive REPL for the system agent: cos agent chat [--session <id>] [--no-stream] [--no-memory] [--show-tools] [--max-turns N] (slash commands: /quit /help /session /clear /history [N] /tools). For one-shot App-gated calls use `cos ai chat --app <id>` — `cos agent chat` is the kernel Agent's own surface and is not an App entry point."),
            ("serve", "Boot the built-in web UI on http://127.0.0.1:7878 (override with --bind / --port). Chat streams over Server-Sent Events; tasks / approvals / inbox / sysinfo are JSON endpoints. Access is token-gated (persisted at $COS_DATA_DIR/agent/web/serve.token). Add --open to launch a browser. Designed for WSL and headless-Linux hosts where the terminal can't render the same surface."),
            ("budget", "Inspect or reset an app's monthly AI budget: cos agent budget show|reset|history <app>. The system agent reports under the pseudo-app id `system.agent`."),
            ("status", "Short live verdict: provider/model/key source, ready/not-ready, most-recent session. Use `cos agent doctor` for the full provider matrix, tool list, skills, usage."),
            ("sessions", "Inspect / manage conversation sessions in the memory DB: cos agent sessions [list [N] | title <id> | set-title <id> \"<title>\" | count [<id>] | clear <id> --yes]"),
            ("recall", "FTS5 search across recorded conversations: cos agent recall \"<query>\" [limit]"),
            ("service", "Daemon-backed task queue: cos agent service {submit \"<prompt>\" | list | status <id> | result <id> | cancel <id>}. Requires clawd."),
            ("notes", "Manage agent markdown notes (MEMORY.md / USER.md / custom): cos agent notes [list|read <n>|write <n> <content>|append <n> <line>|delete <n>]"),
            ("memory", "Inspect or redact app-emitted memory rows: cos agent memory [list [--source <id>] [--limit N] | show <row_id> | search \"<query>\" [--source <id>] [--limit N] | forget {--row <id> | --source <id>} [--yes]]. Apps push rows via the `memory.write` capability."),
            ("skills", "Inspect or install skill bundles: cos agent skills [list|info <id>|install <archive.zip>|hub <list|show|install> <owner/repo>|...]"),
            ("todo", "Manage per-session agent todo lists: cos agent todo [list <session_id>|add <session_id> <id> <title>|set-status ...|remove ...|clear ...]"),
            ("mcp", "MCP (Model Context Protocol) bridge — server exposes the cos agent tool catalogue; client probes/invokes a remote MCP subprocess"),
            ("doctor", "Aggregate diagnostic — provider config matrix, engines, memory, skills, hooks, audit/run-log + last 7d usage & insights. Add --probe-network for a live provider ping."),
            ("diagnose", "System Doctor: cos agent diagnose [--quick] [--domain <general|performance|network|storage|service|crash|thermal|security>] [--path <path>] \"<symptom>\". Collects structured evidence and returns confidence-linked findings without requiring an LLM."),
            ("ls", "List active / paused / failed agent tasks (durable sessions on disk). Columns: id, purpose, status, current lease holder."),
            ("show", "Show one task in detail: cos agent show <task-id> — purpose, status, lease, turn count, mutation breakdown by kind, stop-requested flag."),
            ("stop", "Politely stop a running task: cos agent stop <task-id> — drops a stop sentinel for the live runtime to notice; if no runtime is attached, flips status to paused immediately."),
            ("undo", "Replay the inverse mutation log to roll a task's filesystem changes back: cos agent undo <task-id> [--dry-run]."),
            ("resume", "Mark a paused task as ready for re-attachment: cos agent resume <task-id>. Does not itself spawn a runtime — `cos agent chat --session <id>` (or another runtime) takes it from there."),
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

// ---------------------------------------------------------------------------
// --schema support: structured parameter introspection for every command
// ---------------------------------------------------------------------------

pub(super) struct CommandSchema {
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

pub(super) fn command_schemas() -> Vec<(&'static str, &'static str, Vec<CommandSchema>)> {
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
                    command: "oauth-login",
                    description: "Open the system browser and complete installed-app OAuth login",
                    params: vec![
                        Param::positional(
                            "provider",
                            "string",
                            true,
                            "OAuth provider (google or microsoft)",
                        ),
                        Param::flag("--namespace", "string", false, "Namespace"),
                        Param::flag("--no-open", "bool", false, "Print URL without opening browser"),
                        Param::flag("--timeout", "integer", false, "Callback timeout in seconds"),
                    ],
                    example: "cos credential oauth-login google",
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

pub(super) fn show_command_schema(app_name: &str, command: &str) -> Result<Option<String>, String> {
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

pub(super) fn show_builtin_schema(app_name: &str) -> Result<Option<String>, String> {
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

pub(super) fn show_app_command_schema(
    app_name: &str,
    command: &str,
    app: &apps::App,
) -> Result<Option<String>, String> {
    let operation = app
        .manifest
        .operations
        .get(command)
        .ok_or_else(|| format!("unknown App operation: {app_name} {command}"))?;
    let schema = apps::operation_schema(operation);
    Ok(Some(
        json!({
            "command": format!("cos app {app_name} {command}"),
            "description": schema["description"].clone(),
            "parameters": schema["parameters"].clone(),
        })
        .to_string(),
    ))
}

pub(super) fn show_app_schema(app_name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let mut commands = Vec::new();
    for (cmd_name, op) in &app.manifest.operations {
        let schema = apps::operation_schema(op);
        let entry = json!({
            "command": cmd_name,
            "label": op.label.current(),
            "description": op.summary.current(),
            "parameters": schema["parameters"].clone(),
        });
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

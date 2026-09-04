//! Shared help and command-schema rendering for terminal and model discovery.

use std::env;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::apps;
use crate::cli_catalog;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn apps_dir() -> PathBuf {
    PathBuf::from(env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

pub(crate) fn show_overview() -> Result<Option<String>, String> {
    // The headline count means "Apps you can run". Quarantined installs
    // are surfaced with their reason by `cos app`, not counted here.
    let output = cli_catalog::overview(VERSION, apps::discover_verified(&apps_dir()).len());
    Ok(Some(output.to_string()))
}

/// `cos help <topic>` — focused help for one primitive or app. Falls
/// back to the global overview when the topic is unknown so the user
/// always sees something useful (and the available names).
pub(crate) fn show_help_for(topic: &str) -> Result<Option<String>, String> {
    // Built-in primitives use the same shape as `cos <primitive>`
    // (no args).
    if let Some(help) = cli_catalog::namespace_help(topic) {
        return Ok(Some(help.to_string()));
    }

    // Apps: render the same help as `cos app <name>`.
    // Help and schema display only: `show_app_help` never executes App
    // code, and a quarantined install must stay visible so the operator
    // can see why it stopped working. Every execution path goes through
    // `apps::find_verified` / `require_runnable` instead.
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

pub(crate) fn show_apps(
    discovered: &std::collections::BTreeMap<String, apps::App>,
) -> Result<Option<String>, String> {
    let mut app_list = Vec::new();
    let mut quarantined = Vec::new();
    for (name, app) in discovered {
        app_list.push(json!({
            "name": name,
            "label": app.manifest.name.current(),
            "description": app.manifest.summary.current(),
            "commands": app_command_labels(app),
            "trust": app.trust_label(),
            "runnable": app.is_verified(),
            "quarantine_reason": app.quarantine_reason(),
        }));
        if let Some(reason) = app.quarantine_reason() {
            quarantined.push(json!({ "name": name, "reason": reason }));
        }
    }

    let output = json!({
        "apps": app_list,
        "total": app_list.len(),
        "quarantined": quarantined,
        "hint": "Run: cos app <name> for app details, cos app <name> <command> [args] to execute. Scaffold a new App with: cos app create <id> [--kind cli|desktop|both]. Install an App with: cos app install <source-dir>",
    });
    Ok(Some(output.to_string()))
}

pub(crate) fn show_app_help(name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let output = json!({
        "app": name,
        "label": app.manifest.name.current(),
        "version": app.manifest.version,
        "description": app.manifest.summary.current(),
        "commands": app_command_labels(app),
        "hint": format!("Run: cos app {name} <command> [args]"),
    });
    Ok(Some(output.to_string()))
}

/// The human `cos app <id> <command>` surface as a `command -> label`
/// map. Legacy operation Apps report their operations; MCP-only Apps
/// report the commands derived from their `<app_id>.<command>` tools.
fn app_command_labels(app: &apps::App) -> serde_json::Map<String, Value> {
    if apps::is_mcp_only_cli(&app.manifest) {
        mcp_cli_commands(&app.manifest)
            .into_iter()
            .map(|(command, tool)| (command, json!(tool.summary.current())))
            .collect()
    } else {
        app.manifest
            .operations
            .iter()
            .map(|(k, op)| (k.clone(), json!(op.label.current())))
            .collect()
    }
}

/// Ordered `(command, tool)` pairs an MCP-only App exposes to the human
/// CLI. Only tools that follow the `<app_id>.<command>` convention map
/// to a CLI command; anything else stays agent-only.
fn mcp_cli_commands(
    manifest: &apps::AppManifest,
) -> Vec<(String, &crate::caps::manifest::McpTool)> {
    let prefix = format!("{}.", manifest.id);
    manifest
        .mcp
        .as_ref()
        .map(|service| {
            service
                .tools
                .iter()
                .filter_map(|tool| {
                    tool.name
                        .strip_prefix(&prefix)
                        .filter(|command| !command.is_empty())
                        .map(|command| (command.to_string(), tool))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn builtin_apps() -> Vec<(
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
)> {
    cli_catalog::builtin_namespaces()
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

pub(crate) fn command_schemas() -> Vec<(&'static str, &'static str, Vec<CommandSchema>)> {
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
        (
            "agent",
            "OS-native agent runtime",
            vec![CommandSchema {
                command: "usage",
                description: "Aggregate token usage with optional provider, model, session, App, verb, time, and status filters",
                params: vec![
                    Param::positional(
                        "scope",
                        "enum:overall|provider|model|session|app|verb",
                        false,
                        "Aggregation scope (default: overall)",
                    ),
                    Param::positional(
                        "value",
                        "string",
                        false,
                        "Required after provider, model, session, app, or verb",
                    ),
                    Param::flag(
                        "--since",
                        "RFC3339 timestamp",
                        false,
                        "Inclusive lower timestamp bound",
                    ),
                    Param::flag(
                        "--until",
                        "RFC3339 timestamp",
                        false,
                        "Exclusive upper timestamp bound",
                    ),
                    Param::flag("--ok", "bool", false, "Include only successful calls"),
                    Param::flag("--error", "bool", false, "Include only failed calls"),
                    Param::flag("--app", "string", false, "Filter by App id"),
                    Param::flag("--verb", "string", false, "Filter by AI verb"),
                ],
                example: "cos agent usage overall --since 2026-08-01T00:00:00Z",
            }],
        ),
    ]
}

fn command_schema_value(app_name: &str, command: &str) -> Result<Value, String> {
    let mut output = cli_catalog::command_help(app_name, command)
        .ok_or_else(|| format!("unknown command: cos {app_name} {command}"))?;
    let detailed = command_schemas()
        .into_iter()
        .find(|(name, _, _)| *name == app_name)
        .and_then(|(_, _, commands)| commands.into_iter().find(|entry| entry.command == command));
    let Some(object) = output.as_object_mut() else {
        return Err("command catalogue produced a non-object entry".to_string());
    };
    let Some(cmd) = detailed else {
        object.insert("schema_available".into(), json!(false));
        object.insert("parameters".into(), Value::Null);
        object.insert("example".into(), Value::Null);
        return Ok(output);
    };
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
    object.insert("description".into(), json!(cmd.description));
    object.insert("schema_available".into(), json!(true));
    object.insert("parameters".into(), json!(params));
    object.insert("example".into(), json!(cmd.example));
    Ok(output)
}

pub(crate) fn show_command_schema(app_name: &str, command: &str) -> Result<Option<String>, String> {
    Ok(Some(command_schema_value(app_name, command)?.to_string()))
}

pub(crate) fn show_builtin_schema(app_name: &str) -> Result<Option<String>, String> {
    let names =
        cli_catalog::command_names(app_name).ok_or_else(|| format!("no schema for: {app_name}"))?;
    let commands: Vec<Value> = names
        .into_iter()
        .map(|command| command_schema_value(app_name, command))
        .collect::<Result<_, _>>()?;
    let description = cli_catalog::namespace_help(app_name)
        .and_then(|value| value.get("description").cloned())
        .unwrap_or(Value::Null);

    let output = json!({
        "app": app_name,
        "description": description,
        "commands": commands,
    });
    Ok(Some(output.to_string()))
}

pub(crate) fn show_app_command_schema(
    app_name: &str,
    command: &str,
    app: &apps::App,
) -> Result<Option<String>, String> {
    // Staged migration gate: an App with no operations but an MCP service
    // resolves its CLI schema from the manifest-declared tool named by the
    // `<app_id>.<command>` convention. Schema introspection reads the
    // manifest only and never runs App code.
    let schema = if apps::is_mcp_only_cli(&app.manifest) {
        let tool = apps::mcp_tool_for_command(&app.manifest, command)?;
        apps::tool_schema(tool)
    } else {
        let operation = app
            .manifest
            .operations
            .get(command)
            .ok_or_else(|| format!("unknown App operation: {app_name} {command}"))?;
        apps::operation_schema(operation)
    };
    Ok(Some(
        json!({
            "command": format!("cos app {app_name} {command}"),
            "description": schema["description"].clone(),
            "parameters": schema["parameters"].clone(),
            "stdin": schema["stdin"].clone(),
        })
        .to_string(),
    ))
}

pub(crate) fn show_app_schema(app_name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let mut commands = Vec::new();
    if apps::is_mcp_only_cli(&app.manifest) {
        for (command, tool) in mcp_cli_commands(&app.manifest) {
            let schema = apps::tool_schema(tool);
            commands.push(json!({
                "command": command,
                "label": tool.summary.current(),
                "description": tool.summary.current(),
                "parameters": schema["parameters"].clone(),
                "stdin": schema["stdin"].clone(),
            }));
        }
    } else {
        for (cmd_name, op) in &app.manifest.operations {
            let schema = apps::operation_schema(op);
            commands.push(json!({
                "command": cmd_name,
                "label": op.label.current(),
                "description": op.summary.current(),
                "parameters": schema["parameters"].clone(),
                "stdin": schema["stdin"].clone(),
            }));
        }
    }

    let output = json!({
        "app": app_name,
        "label": app.manifest.name.current(),
        "description": app.manifest.summary.current(),
        "commands": commands,
    });
    Ok(Some(output.to_string()))
}

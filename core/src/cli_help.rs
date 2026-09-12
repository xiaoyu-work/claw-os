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
    let cmds: serde_json::Map<String, Value> = app
        .manifest
        .operations
        .iter()
        .map(|(k, op)| (k.clone(), json!(op.label.current())))
        .collect();
    let mut output = json!({
        "app": name,
        "label": app.manifest.name.current(),
        "version": app.manifest.version,
        "description": app.manifest.summary.current(),
        "commands": cmds,
        "hint": format!("Run: cos app {name} <command> [args]"),
    });
    append_object_schema(&mut output, app);
    Ok(Some(output.to_string()))
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
            "activity",
            "Owner-scoped goals shared by terminal, Web, and desktop",
            activity_schemas(),
        ),
        (
            "object",
            "Authenticated App object contracts and explicit resolution",
            object_schemas(),
        ),
        (
            "operation",
            "Non-executing App effect previews",
            vec![CommandSchema {
                command: "preview",
                description: "Preview App declarations, never authorize or execute them",
                params: vec![
                    Param::positional("app", "string", true, "Installed App ID"),
                    Param::positional("operation", "string", true, "Declared operation"),
                    Param::flag("--activity", "uuid", false, "Owner-scoped Activity context"),
                    Param::positional("args", "array<string>", false, "App arguments after --; not executed"),
                ],
                example: "cos operation preview fs write -- /home/user/draft.md --content Draft",
            }, CommandSchema {
                command: "execute",
                description: "Execute through the normal App gate and record an unverified outcome report",
                params: vec![
                    Param::positional("app", "string", true, "Installed App ID"),
                    Param::positional("operation", "string", true, "Declared operation"),
                    Param::flag("--activity", "uuid", true, "Active owner-scoped Activity"),
                    Param::positional("args", "array<string>", false, "App arguments after --; stdin is not forwarded"),
                ],
                example: "cos operation execute kv get --activity 00000000-0000-4000-8000-000000000001 -- release.status",
            }],
        ),
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

fn activity_schemas() -> Vec<CommandSchema> {
    let metadata = |goal_required| {
        vec![
            Param::flag("--goal", "string", goal_required, "Desired outcome"),
            Param::flag(
                "--criteria",
                "string",
                false,
                "How goal achievement will be confirmed",
            ),
            Param::flag(
                "--boundaries",
                "string",
                false,
                "Planning constraints; does not grant additional permissions",
            ),
            Param::flag(
                "--resource",
                "string",
                false,
                "LABEL=REFERENCE (repeatable; replaces the resource list)",
            ),
        ]
    };
    let id = || Param::positional("id", "uuid", true, "Activity ID");
    let state_entry = || {
        vec![
            id(),
            Param::flag(
                "--reference",
                "string",
                true,
                "Existing canonical App resource reference",
            ),
            Param::flag(
                "--id",
                "uuid",
                false,
                "Idempotency key; reuse it when retrying a submission",
            ),
            Param::flag(
                "--supersedes",
                "uuid",
                false,
                "Current entry to correct; history is preserved",
            ),
        ]
    };
    let mut observation = state_entry();
    observation.extend([
        Param::flag(
            "--source",
            "enum:user_statement|agent_inference",
            false,
            "Reported classification; not proof of authorship or truth",
        ),
        Param::flag(
            "--text",
            "string",
            false,
            "Statement or inference (required unless linking a receipt)",
        ),
        Param::flag(
            "--receipt",
            "uuid",
            false,
            "Link an existing Activity/App receipt instead of supplying text",
        ),
        Param::flag(
            "--observed-at",
            "RFC3339 timestamp",
            false,
            "Reported window start; requires --valid-until",
        ),
        Param::flag(
            "--valid-until",
            "RFC3339 timestamp",
            false,
            "Reported window end; not verified freshness",
        ),
    ]);
    let mut relation = state_entry();
    relation.extend([
        Param::flag(
            "--target",
            "string",
            true,
            "Another attached canonical App reference",
        ),
        Param::flag(
            "--relation",
            "enum:related_to|depends_on|derived_from",
            true,
            "Planning relation; never an execution dependency",
        ),
        Param::flag("--note", "string", false, "Optional relationship note"),
    ]);
    let mut retraction = state_entry();
    retraction.push(Param::flag(
        "--reason",
        "string",
        true,
        "Why the --supersedes entry is retracted",
    ));
    let mut create = vec![Param::positional("title", "string", true, "Activity title")];
    create.extend(metadata(true));
    let mut update = vec![
        id(),
        Param::flag("--title", "string", false, "Replacement title"),
    ];
    update.extend(metadata(false));
    update.push(Param::flag(
        "--clear-resources",
        "bool",
        false,
        "Remove references without deleting the referenced data",
    ));
    let mut schemas = vec![
        CommandSchema {
            command: "execution-limits",
            description: "Read Activity attempt/turn/expiry controls; no capabilities are granted",
            params: vec![id()],
            example: "cos activity execution-limits 00000000-0000-4000-8000-000000000001",
        },
        CommandSchema {
            command: "set-execution-limits",
            description: "Create or revise finite Activity limits without resetting usage or enabling a disabled policy",
            params: vec![
                id(),
                Param::flag("--revision", "integer", false, "Current revision when updating; omit only for initial creation"),
                Param::flag("--max-attempts", "integer", true, "Lifetime attempt ceiling, 1-1000"),
                Param::flag("--max-turns", "integer", true, "Maximum model turns per attempt, 1-100"),
                Param::flag("--expires-at", "RFC3339 timestamp", true, "Future expiry of this limit policy"),
            ],
            example: "cos activity set-execution-limits 00000000-0000-4000-8000-000000000001 --max-attempts 10 --max-turns 5 --expires-at 2026-09-18T00:00:00Z",
        },
        CommandSchema {
            command: "enable-execution-limits",
            description: "Explicitly enable an unexpired policy at the expected revision",
            params: vec![id(), Param::flag("--revision", "integer", true, "Current policy revision")],
            example: "cos activity enable-execution-limits 00000000-0000-4000-8000-000000000001 --revision 2",
        },
        CommandSchema {
            command: "disable-execution-limits",
            description: "Disable bounded work without deleting its policy, clearing usage or granting unlimited work",
            params: vec![id(), Param::flag("--revision", "integer", true, "Current policy revision")],
            example: "cos activity disable-execution-limits 00000000-0000-4000-8000-000000000001 --revision 2",
        },
        CommandSchema {
            command: "object-state",
            description: "Read caller-reported object state, time windows, relations and correction history",
            params: vec![
                id(),
                Param::flag("--reference", "string", false, "Filter by exact attached App reference"),
                Param::flag("--limit", "integer", false, "Maximum entries, 1-100 (default 50)"),
            ],
            example: "cos activity object-state 00000000-0000-4000-8000-000000000001",
        },
        CommandSchema {
            command: "observe",
            description: "Annotate an attached object without fetching its data or granting authority",
            params: observation,
            example: "cos activity observe 00000000-0000-4000-8000-000000000001 --reference 'app://kv/entry?id=release.status' --text 'Waiting for review'",
        },
        CommandSchema {
            command: "relate",
            description: "Add a non-executing planning relationship between attached objects",
            params: relation,
            example: "cos activity relate 00000000-0000-4000-8000-000000000001 --reference 'app://kv/entry?id=release.status' --target 'app://kv/entry?id=review.status' --relation depends_on",
        },
        CommandSchema {
            command: "retract-object-state",
            description: "Supersede an entry with an explicit retraction while preserving history",
            params: retraction,
            example: "cos activity retract-object-state 00000000-0000-4000-8000-000000000001 --reference 'app://kv/entry?id=release.status' --supersedes 00000000-0000-4000-8000-000000000002 --reason 'No longer supported'",
        },
        CommandSchema {
            command: "record-object-state",
            description: "Submit the same bounded object-state draft used by graphical clients",
            params: vec![
                id(),
                Param::flag("--stdin", "bool", true, "Read at most 16 KiB of entry JSON from piped stdin"),
            ],
            example: "cos activity record-object-state 00000000-0000-4000-8000-000000000001 --stdin < entry.json",
        },
        CommandSchema {
            command: "receipts",
            description: "Read caller-reported results without inferring goal completion or verified effects",
            params: vec![
                id(),
                Param::flag("--limit", "integer", false, "Maximum receipts, 1-100 (default 50)"),
            ],
            example: "cos activity receipts 00000000-0000-4000-8000-000000000001",
        },
        CommandSchema {
            command: "record-receipt",
            description: "Retry recording a bounded report from stdin without re-executing the operation",
            params: vec![
                id(),
                Param::flag("--stdin", "bool", true, "Read at most 16 KiB of report JSON from piped stdin"),
            ],
            example: "cos activity record-receipt 00000000-0000-4000-8000-000000000001 --stdin < report.json",
        },
        CommandSchema {
            command: "objects",
            description: "Describe attached App references without reading their data",
            params: vec![id()],
            example: "cos activity objects 00000000-0000-4000-8000-000000000001",
        },
        CommandSchema {
            command: "attach-object",
            description: "Atomically attach a declared App object without invoking it",
            params: vec![
                id(),
                Param::flag("--label", "string", true, "User-facing label"),
                Param::flag("--app", "string", true, "Installed App ID"),
                Param::flag("--type", "string", true, "Object type declared by the App"),
                Param::flag("--object-id", "string", true, "Opaque App-owned object ID"),
                Param::flag("--revision", "string", false, "Optional App revision constraint"),
            ],
            example: "cos activity attach-object 00000000-0000-4000-8000-000000000001 --label Release --app kv --type entry --object-id release.status",
        },
        CommandSchema {
            command: "create",
            description: "Create a persistent Activity without starting a task",
            params: create,
            example: "cos activity create \"Release v2\" --goal \"Publish next Friday\"",
        },
        CommandSchema {
            command: "list",
            description: "List only the authenticated owner's Activities",
            params: vec![
                Param::flag(
                    "--state",
                    "enum:active|paused|completed|cancelled",
                    false,
                    "Filter by Activity lifecycle state",
                ),
                Param::flag("--limit", "integer", false, "Maximum records, 1-100 (default 50)"),
            ],
            example: "cos activity list --state active",
        },
        CommandSchema {
            command: "show",
            description: "Read a shared Activity view with bounded recent job results",
            params: vec![
                id(),
                Param::flag("--limit", "integer", false, "Maximum related jobs, 1-100 (default 50)"),
            ],
            example: "cos activity show 00000000-0000-4000-8000-000000000001",
        },
        CommandSchema {
            command: "update",
            description: "Change supplied fields of an active or paused Activity",
            params: update,
            example: "cos activity update 00000000-0000-4000-8000-000000000001 --criteria \"Release is available\"",
        },
        CommandSchema {
            command: "run",
            description: "Submit durable work using the Activity goal or an explicit prompt",
            params: vec![
                id(),
                Param::positional("prompt", "string", false, "Work request (default: Activity goal)"),
                Param::flag("--session", "string", false, "Continue an associated session"),
                Param::flag("--max-turns", "integer", false, "Positive per-job model-turn limit"),
            ],
            example: "cos activity run 00000000-0000-4000-8000-000000000001 \"Prepare a release draft\"",
        },
        CommandSchema {
            command: "complete",
            description: "Explicitly confirm that the goal has been achieved",
            params: vec![
                id(),
                Param::flag("--note", "string", true, "User confirmation of the achieved outcome"),
            ],
            example: "cos activity complete 00000000-0000-4000-8000-000000000001 --note \"Reviewed and published\"",
        },
    ];
    for (command, description, example) in [
        (
            "pause",
            "Prevent future work without undoing in-flight effects",
            "cos activity pause 00000000-0000-4000-8000-000000000001",
        ),
        (
            "resume",
            "Resume or explicitly reopen an Activity",
            "cos activity resume 00000000-0000-4000-8000-000000000001",
        ),
        (
            "cancel",
            "End an Activity without claiming that its goal was achieved",
            "cos activity cancel 00000000-0000-4000-8000-000000000001",
        ),
    ] {
        schemas.push(CommandSchema {
            command,
            description,
            params: vec![id()],
            example,
        });
    }
    schemas
}

fn object_schemas() -> Vec<CommandSchema> {
    vec![
        CommandSchema {
            command: "catalog",
            description: "List verified App object declarations without executing Apps",
            params: vec![Param::positional("app", "string", false, "Optional App ID")],
            example: "cos object catalog kv",
        },
        CommandSchema {
            command: "reference",
            description:
                "Create a canonical reference; this does not prove existence or grant access",
            params: vec![
                Param::positional("app", "string", true, "App ID"),
                Param::positional("type", "string", true, "App-declared object type"),
                Param::positional("id", "string", true, "Opaque object ID"),
                Param::flag(
                    "--revision",
                    "string",
                    false,
                    "Optional revision constraint",
                ),
            ],
            example: "cos object reference kv entry release.status",
        },
        CommandSchema {
            command: "describe",
            description: "Read authenticated declaration and invocation metadata, not object data",
            params: vec![Param::positional(
                "reference",
                "string",
                true,
                "Canonical App object URI",
            )],
            example: "cos object describe 'app://kv/entry?id=release.status'",
        },
        CommandSchema {
            command: "resolve",
            description:
                "Execute the declared App operation with its ordinary permissions and audit",
            params: vec![Param::positional(
                "reference",
                "string",
                true,
                "Canonical App object URI",
            )],
            example: "cos object resolve 'app://kv/entry?id=release.status'",
        },
    ]
}

fn append_object_schema(output: &mut Value, app: &apps::App) {
    match apps::verified_object_schema(app) {
        Ok(objects)
            if objects
                .as_object()
                .is_some_and(|objects| !objects.is_empty()) =>
        {
            output["objects"] = objects;
        }
        Ok(_) => {}
        Err(error) => output["objects_error"] = json!(error),
    }
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
            "stdin": schema["stdin"].clone(),
        })
        .to_string(),
    ))
}

pub(crate) fn show_app_schema(app_name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let mut commands = Vec::new();
    for (cmd_name, op) in &app.manifest.operations {
        let schema = apps::operation_schema(op);
        let entry = json!({
            "command": cmd_name,
            "label": op.label.current(),
            "description": op.summary.current(),
            "parameters": schema["parameters"].clone(),
            "stdin": schema["stdin"].clone(),
        });
        commands.push(entry);
    }

    let mut output = json!({
        "app": app_name,
        "label": app.manifest.name.current(),
        "description": app.manifest.summary.current(),
        "commands": commands,
    });
    append_object_schema(&mut output, app);
    Ok(Some(output.to_string()))
}

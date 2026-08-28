//! Machine-readable public `cos` command catalogue.
//!
//! This is a definition layer shared by terminal help and model-facing
//! progressive discovery. It describes commands but never dispatches them, so
//! browsing the tree cannot cross a capability, approval, or audit boundary.

use serde_json::{json, Map, Value};

pub type CommandEntry = (&'static str, &'static str);
pub type NamespaceEntry = (&'static str, &'static str, Vec<CommandEntry>);

/// Public CLI namespaces and their canonical commands.
///
/// Aliases are intentionally omitted. A caller discovers one stable spelling,
/// while the command implementation may continue accepting compatibility
/// aliases.
pub fn builtin_namespaces() -> Vec<NamespaceEntry> {
    vec![
        (
            "sys",
            "System information - hardware, OS, environment, resources, and live Linux telemetry",
            vec![
                ("info", "Get host distribution, architecture, hostname, and Claw Agent identity"),
                ("env", "List environment variables, optionally filtered by pattern"),
                ("uptime", "Show system uptime"),
                ("who", "List logged-in users"),
                ("desktop", "Inspect the active desktop session"),
                ("resources", "Show disk, memory, and CPU usage"),
                ("loadavg", "Show system load averages"),
                ("sensors", "Read available hardware sensors"),
                ("cgroup", "Show cgroup v2 limits and usage"),
                ("proc", "List processes with CPU and memory data"),
                ("top", "Sample the busiest processes"),
                ("threads", "List threads for one process"),
                ("port", "Find listeners and connections for one port"),
                ("net", "Show network interfaces and connections"),
                ("net_rate", "Sample network throughput"),
                ("mounts", "List mounted filesystems"),
                ("disk_io", "Sample block-device I/O"),
                ("largest_files", "Find the largest files below a path"),
                ("journal", "Read bounded system journal entries"),
                ("dmesg", "Read bounded kernel log entries"),
                ("services", "List systemd services"),
                ("failed_units", "List failed systemd units"),
                ("coredumps", "List recent systemd coredumps"),
                ("pkg_updates", "List available package updates"),
            ],
        ),
        (
            "service",
            "Generic service manager - lifecycle hooks, graceful shutdown, and dependency ordering",
            vec![
                ("start", "Start a managed service"),
                ("stop", "Gracefully stop a managed service"),
                ("stop-all", "Stop every managed service in reverse dependency order"),
                ("restart", "Restart a managed service"),
                ("status", "Inspect one service and its recent output"),
                ("health", "Run a service health check"),
                ("list", "List managed services"),
                ("logs", "Read bounded service logs"),
                ("register", "Register a managed service"),
            ],
        ),
        (
            "checkpoint",
            "OverlayFS checkpoint system - snapshot, diff, rollback, quota, and namespaces",
            vec![
                ("create", "Freeze current changes into a named checkpoint"),
                ("diff", "Show created, modified, and deleted files"),
                ("rollback", "Restore a checkpoint or reset to base"),
                ("list", "List saved checkpoints"),
                ("status", "Show overlay state and disk usage"),
                ("quota-set", "Set the upper-layer filesystem quota"),
                ("quota-status", "Show quota usage"),
                ("namespaces", "Manage isolated overlay namespaces"),
            ],
        ),
        (
            "credential",
            "Encrypted credential store - namespaces, TTL, OAuth refresh, and bundles",
            vec![
                ("store", "Store a credential"),
                ("load", "Load a credential after policy checks"),
                ("revoke", "Delete a stored credential"),
                ("list", "List credential metadata without values"),
                ("bundle", "Create a named credential bundle"),
                ("load-bundle", "Load a credential bundle after policy checks"),
                ("oauth-login", "Complete Google PKCE or Microsoft device-code login"),
                ("oauth-refresh", "Refresh an existing Google or Microsoft login"),
            ],
        ),
        (
            "cron",
            "Agent-native recurring job scheduler",
            vec![
                ("add", "Register a cron job"),
                ("remove", "Remove a cron job"),
                ("list", "List cron jobs"),
                ("status", "Inspect one cron job"),
                ("enable", "Enable a cron job"),
                ("disable", "Disable a cron job"),
                ("logs", "Read cron execution history"),
                ("run", "Run a cron job immediately"),
                ("tick", "Process due cron jobs"),
            ],
        ),
        (
            "triggers",
            "Event-driven agent job triggers",
            vec![
                ("add", "Register an event trigger"),
                ("list", "List event triggers"),
                ("remove", "Remove an event trigger"),
                ("enable", "Enable an event trigger"),
                ("disable", "Disable an event trigger"),
                ("run", "Run a trigger immediately"),
                ("tick", "Evaluate pending trigger events"),
            ],
        ),
        (
            "ai",
            "App-facing AI gate for capability-scoped model and tool calls",
            vec![
                ("chat", "Run one App-gated AI request"),
                ("tool", "Invoke one App-facing AI tool"),
                ("tools", "List the App-facing AI tool catalogue"),
            ],
        ),
        (
            "agent",
            "OS-native agent runtime, memory, Skills, providers, usage, and tasks",
            vec![
                ("setup", "Configure text, speech, image, and embedding providers"),
                ("ask", "Run one prompt through the full tool and memory loop"),
                ("chat", "Open the interactive Agent REPL"),
                ("serve", "Run the authenticated local Agent web UI"),
                ("budget", "Inspect App and per-user AI budget units"),
                ("override", "Inspect per-user App AI overrides"),
                ("status", "Show the active provider and Agent readiness"),
                (
                    "usage",
                    "Inspect token usage by provider, model, session, App, verb, time range, or status",
                ),
                ("sessions", "Inspect and manage recorded conversations"),
                ("recall", "Search recorded conversations"),
                ("service", "Inspect and manage daemon-backed Agent tasks"),
                ("notes", "Manage Agent memory notes"),
                ("memory", "Inspect or forget App-emitted memory"),
                ("skills", "Inspect and install Agent Skills"),
                ("todo", "Manage per-session Agent todo lists"),
                ("mcp", "Inspect and operate MCP integrations"),
                (
                    "doctor",
                    "Run the holistic Agent self-check, including recent token usage",
                ),
                ("diagnose", "Run deterministic system diagnosis"),
                ("ls", "List durable Agent tasks"),
                ("show", "Show one durable Agent task"),
                ("stop", "Request a running Agent task to stop"),
                ("undo", "Roll back a task's recorded filesystem mutations"),
                ("resume", "Mark a paused Agent task ready for re-attachment"),
                ("dev", "Inspect unstable power-user and internal diagnostics"),
            ],
        ),
        (
            "model",
            "Local model registry and inference runtime",
            vec![
                ("list", "List registered models"),
                ("import", "Import an ONNX or GGUF model"),
                ("load", "Load a registered model"),
                ("unload", "Unload a model"),
                ("infer", "Run local inference"),
                ("embed", "Generate embeddings"),
                ("image", "Generate an image"),
                ("transcribe", "Transcribe audio"),
                ("translate", "Translate audio into English text"),
                ("speak", "Synthesize speech"),
                ("status", "Inspect model runtime state"),
                ("bench", "Benchmark a model"),
                ("rm", "Remove a registered model"),
            ],
        ),
        (
            "engine",
            "Native inference engine package manager",
            vec![
                ("list", "List engines or inspect one installed engine"),
                ("update", "Install or update an engine"),
                ("activate", "Activate one installed engine version"),
                ("remove", "Remove a version or garbage-collect old versions"),
                ("unpin", "Allow future updates to change the active version"),
            ],
        ),
    ]
}

pub fn overview(version: &str, apps_available: usize) -> Value {
    let primitives: Vec<Value> = builtin_namespaces()
        .into_iter()
        .map(|(name, description, commands)| {
            json!({
                "name": name,
                "description": description,
                "commands_available": commands.len(),
                "next": format!("cos {name}"),
            })
        })
        .collect();
    json!({
        "name": "cos",
        "version": version,
        "description": "Claw OS - agent-native operating system. All commands return structured JSON.",
        "primitives": primitives,
        "total_primitives": primitives.len(),
        "apps_available": apps_available,
        "next": {
            "primitive": "cos <primitive>",
            "apps": "cos app",
        },
    })
}

pub fn namespace_help(name: &str) -> Option<Value> {
    let (_, description, commands) = builtin_namespaces()
        .into_iter()
        .find(|(namespace, _, _)| *namespace == name)?;
    let command_map: Map<String, Value> = commands
        .iter()
        .map(|(command, summary)| (command.to_string(), json!(summary)))
        .collect();
    let model_tools: Map<String, Value> = commands
        .iter()
        .filter_map(|(command, _)| {
            model_tool_for(name, command)
                .map(|tool| (command.to_string(), Value::String(tool.to_string())))
        })
        .collect();
    Some(json!({
        "app": name,
        "description": description,
        "commands": command_map,
        "model_tools": model_tools,
        "hint": format!("Inspect one command with: cos {name} <command> --schema"),
    }))
}

pub fn command_help(namespace: &str, command: &str) -> Option<Value> {
    let (_, _, commands) = builtin_namespaces()
        .into_iter()
        .find(|(name, _, _)| *name == namespace)?;
    let (_, description) = commands.into_iter().find(|(name, _)| *name == command)?;
    let model_tool = model_tool_for(namespace, command);
    Some(json!({
        "command": format!("cos {namespace} {command}"),
        "description": description,
        "model_callable": model_tool.is_some(),
        "model_tool": model_tool,
    }))
}

pub fn namespace_names() -> Vec<&'static str> {
    builtin_namespaces()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

pub fn command_names(namespace: &str) -> Option<Vec<&'static str>> {
    builtin_namespaces()
        .into_iter()
        .find(|(name, _, _)| *name == namespace)
        .map(|(_, _, commands)| commands.into_iter().map(|(name, _)| name).collect())
}

pub fn nested_commands(path: &[&str]) -> Option<Vec<CommandEntry>> {
    let commands = match path {
        ["agent", "usage"] => vec![
            ("overall", "Aggregate every matching AI call"),
            ("provider", "Filter by provider name"),
            ("model", "Filter by model id"),
            ("session", "Filter by Agent session id"),
            ("app", "Filter by App id"),
            ("verb", "Filter by AI capability verb"),
        ],
        ["agent", "budget"] => vec![
            ("show", "Show one App's current budget period"),
            ("reset", "Reset one App's budget period"),
            ("history", "List one App's previous budget periods"),
            ("user", "Inspect the aggregate per-user budget"),
        ],
        ["agent", "budget", "user"] => vec![
            ("show", "Show the aggregate per-user budget"),
            ("path", "Show the per-user budget configuration path"),
        ],
        ["agent", "override"] => vec![
            ("show", "Show one App's stored override"),
            ("path", "Show one App's override path"),
            ("effective", "Show one App's effective AI policy"),
        ],
        ["agent", "sessions"] => vec![
            ("list", "List recorded conversations"),
            ("title", "Read one session title"),
            ("set-title", "Set one session title"),
            ("count", "Count messages in one session"),
            ("clear", "Clear one session"),
            ("purge", "Purge old sessions"),
            ("stats", "Show session storage statistics"),
            ("top", "List sessions by message count"),
        ],
        ["agent", "service"] => vec![
            ("submit", "Submit one daemon-backed Agent task"),
            ("list", "List Agent tasks"),
            ("status", "Inspect the daemon or one task"),
            ("result", "Wait for one task result"),
            ("cancel", "Cancel one task"),
            ("context", "Inspect the current system context snapshot"),
            ("events", "Query context events"),
            ("operations", "Query recent system operations"),
        ],
        ["agent", "notes"] => vec![
            ("list", "List Agent notes"),
            ("read", "Read one Agent note"),
            ("write", "Replace one Agent note"),
            ("append", "Append to one Agent note"),
            ("delete", "Delete one Agent note"),
        ],
        ["agent", "memory"] => vec![
            ("list", "List App-emitted memory rows"),
            ("show", "Show one memory row"),
            ("search", "Search App-emitted memory"),
            ("forget", "Delete selected App-emitted memory"),
        ],
        ["agent", "skills"] => vec![
            ("root", "Show the user Skill root"),
            ("list", "List installed Skills"),
            ("info", "Show one Skill's metadata"),
            ("disabled", "List disabled Skills"),
            ("errors", "List Skill load errors"),
            ("install", "Install a Skill bundle"),
            ("hub", "Browse or install a GitHub-hosted Skill"),
            ("usage", "Inspect Skill disclosure usage"),
            ("guard", "Inspect Skill disclosure policy"),
        ],
        ["agent", "skills", "hub"] => vec![
            ("list", "List Skills in a GitHub repository"),
            ("show", "Show one remote Skill"),
            ("install", "Install one remote Skill"),
        ],
        ["agent", "skills", "usage"] => vec![
            ("stats", "Aggregate Skill invocation usage"),
            ("record", "Append one external Skill usage record"),
            ("path", "Show the Skill usage log path"),
            ("clear", "Clear the Skill usage log after confirmation"),
        ],
        ["agent", "todo"] => vec![
            ("path", "Show one session todo path"),
            ("list", "List session todo items"),
            ("add", "Add a todo item"),
            ("set-status", "Change a todo status"),
            ("remove", "Remove a todo item"),
            ("clear", "Clear a session todo list"),
        ],
        ["agent", "mcp"] => vec![
            ("status", "Show MCP configuration and attached tools"),
            ("servers", "Inspect configured MCP servers"),
            ("probe", "Probe an MCP server"),
            ("call", "Invoke a remote MCP tool"),
            ("serve", "Expose the Agent tool registry as an MCP server"),
        ],
        _ => return None,
    };
    Some(commands)
}

fn model_tool_for(namespace: &str, command: &str) -> Option<&'static str> {
    match namespace {
        "sys" => Some("cos_sysinfo"),
        "service" => Some("cos_service"),
        "checkpoint" => Some("cos_checkpoint"),
        "credential" if command == "oauth-login" => Some("cos_oauth_login"),
        "credential" => Some("cos_credential"),
        "cron" => Some("cos_cron"),
        "model"
            if matches!(
                command,
                "list" | "import" | "load" | "unload" | "infer" | "status" | "bench" | "rm"
            ) =>
        {
            Some("cos_model")
        }
        "agent" => match command {
            "doctor" => Some("cos_doctor"),
            "diagnose" => Some("cos_diagnose"),
            "usage" => Some("cos_usage"),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/cli_catalog.rs"
    ));
}

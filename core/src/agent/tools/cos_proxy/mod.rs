//! cos primitive proxy tools.
//!
//! Every cos kernel primitive (sandbox, proc, sysinfo, credential, cron,
//! checkpoint, service, ...) exposes the same uniform contract:
//!
//! ```ignore
//! pub fn run(command: &str, args: &[String]) -> Result<serde_json::Value, String>;
//! ```
//!
//! That contract is identical to how the cos CLI is dispatched. This module
//! lifts each of those primitives into an LLM-callable [`Tool`] with one
//! function pointer per primitive — no hand-written wrappers, no per-tool
//! IPC.
//!
//! The agent therefore inherits the **full** kernel surface: anything
//! callable from the cos CLI is callable by the model, with the same
//! command/args shape. This is the "agent-native OS" promise: the
//! agent is a kernel resident with native access, not a bolt-on
//! layer.
//!
//! Beyond the uniform-contract proxies, this module also hosts higher-level
//! tools backed by agent subsystems (e.g. `cos_memory` over
//! [`crate::agent::memory::notes`]).
//!
//! Phase 5 will layer approval, redaction, and per-tool guardrails on top.
//! For now, calls are dispatched directly. The `policy` module already
//! self-polices destructive operations at the primitive layer.

pub mod memory;
pub mod recall;
pub mod recall_semantic;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

/// Function pointer matching the uniform cos primitive `run` signature.
pub type PrimitiveFn = fn(&str, &[String]) -> Result<Value, String>;

/// Generic LLM-facing wrapper around a cos primitive.
pub struct CosPrimitiveTool {
    name: &'static str,
    description: &'static str,
    primitive: PrimitiveFn,
    /// Allowed `command` values (also documented in the schema enum).
    commands: &'static [&'static str],
    /// Whether the underlying primitive is entirely read-only. The
    /// runtime dispatch loop checks [`Tool::parallel_safe`] to decide
    /// whether to fan this call out concurrently with siblings in the
    /// same turn. Primitives that mix reads and writes leave this
    /// `false`; the LLM still rarely fires more than one such call at
    /// once, and serial dispatch is the safe default.
    parallel_safe: bool,
}

impl CosPrimitiveTool {
    pub const fn new(
        name: &'static str,
        description: &'static str,
        primitive: PrimitiveFn,
        commands: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            description,
            primitive,
            commands,
            parallel_safe: false,
        }
    }

    /// Variant constructor for primitives whose every command is
    /// read-only (e.g. `cos_sysinfo`). The dispatch loop may run
    /// these concurrently with other parallel-safe calls.
    pub const fn new_readonly(
        name: &'static str,
        description: &'static str,
        primitive: PrimitiveFn,
        commands: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            description,
            primitive,
            commands,
            parallel_safe: true,
        }
    }
}

#[async_trait]
impl Tool for CosPrimitiveTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Subcommand to dispatch on this cos primitive.",
                    "enum": self.commands,
                },
                "args": {
                    "type": "array",
                    "description": "Positional / flag args, exactly as you would type after `cos <primitive> <command>`.",
                    "items": { "type": "string" },
                    "default": [],
                }
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return ToolResult::err(format!(
                    "missing 'command' field. valid commands for {}: {:?}",
                    self.name, self.commands
                ));
            }
        };

        let args: Vec<String> = input
            .get("args")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // cos primitives are sync and may do file IO / spawn processes.
        // Run them on the blocking pool so we don't stall the async runtime.
        let primitive = self.primitive;
        let join = tokio::task::spawn_blocking(move || primitive(&command, &args)).await;

        match join {
            Ok(Ok(value)) => {
                let serialized =
                    serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
                ToolResult::ok(serialized)
            }
            Ok(Err(message)) => ToolResult::err(message),
            Err(join_err) => ToolResult::err(format!("primitive panicked: {join_err}")),
        }
    }

    fn parallel_safe(&self) -> bool {
        self.parallel_safe
    }
}

/// Tool descriptor — name, human description, primitive entry point, and the
/// list of commands the primitive understands. Keep this list in sync with
/// each primitive's `run` `match` arms.
///
/// `parallel_safe` opts the primitive into concurrent dispatch with
/// other parallel-safe tools in the same turn. Set to `true` only
/// when every listed command is read-only.
struct PrimitiveSpec {
    name: &'static str,
    description: &'static str,
    primitive: PrimitiveFn,
    commands: &'static [&'static str],
    parallel_safe: bool,
}

const PRIMITIVES: &[PrimitiveSpec] = &[
    PrimitiveSpec {
        name: "cos_sandbox",
        description: "Run commands inside a lightweight Linux-namespace sandbox \
                      (PID/mount/optional network isolation, mem/cpu/pids limits, \
                      seccomp profile). Use for any user-supplied or \
                      model-generated shell command.",
        primitive: crate::sandbox::run,
        commands: &["exec"],
        parallel_safe: false, // runs arbitrary commands; never parallel.
    },
    PrimitiveSpec {
        name: "cos_proc",
        description: "Manage long-running processes registered with cos: spawn, \
                      query status/output, kill/signal, list, wait, renice, \
                      stats, result.",
        primitive: crate::proc::run,
        commands: &[
            "spawn", "status", "output", "kill", "list", "wait", "signal", "result", "stats",
            "renice",
        ],
        parallel_safe: false, // includes spawn/kill/signal.
    },
    PrimitiveSpec {
        name: "cos_sysinfo",
        description: "Read live Linux system telemetry. Commands:\n\
                      identity: info | env | uptime | who | desktop;\n\
                      load: resources | loadavg | sensors | cgroup;\n\
                      processes: proc | top [--top N --by cpu|mem --interval ms] | threads <pid> | port <port>;\n\
                      network: net | net_rate [--interval ms];\n\
                      storage: mounts | disk_io [--interval ms] | largest_files <path> [--top N --min-mb N];\n\
                      logs: journal [--unit X --since X --lines N --priority N --kernel] | dmesg [--lines N];\n\
                      systemd: services [--failed-only --type X --state X] | failed_units | coredumps [--lines N];\n\
                      packages: pkg_updates.",
        primitive: crate::sysinfo::run,
        commands: &[
            "info",
            "env",
            "uptime",
            "who",
            "desktop",
            "resources",
            "loadavg",
            "sensors",
            "cgroup",
            "proc",
            "top",
            "threads",
            "port",
            "net",
            "net_rate",
            "mounts",
            "disk_io",
            "largest_files",
            "journal",
            "dmesg",
            "services",
            "failed_units",
            "coredumps",
            "pkg_updates",
        ],
        // Every command is a pure read of system state. The slow
        // largest_files walk is the canonical reason we want parallel
        // dispatch — the agent typically pairs it with other reads.
        parallel_safe: true,
    },
    PrimitiveSpec {
        name: "cos_credential",
        description: "Manage the cos credential store (file-locked JSON, OS \
                      keychain on supported platforms). Bundle import/export \
                      and OAuth-token refresh supported.",
        primitive: crate::credential::run,
        commands: &[
            "store",
            "load",
            "revoke",
            "list",
            "bundle",
            "load-bundle",
            "oauth-refresh",
        ],
        parallel_safe: false, // mutating store; locks contended on parallel writes.
    },
    PrimitiveSpec {
        name: "cos_cron",
        description: "Schedule recurring jobs. Persists across reboots via cos \
                      service. Cron expression syntax.",
        primitive: crate::cron::run,
        commands: &[
            "add", "remove", "list", "status", "enable", "disable", "logs", "run", "tick",
        ],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_checkpoint",
        description: "Filesystem checkpoints (CoW snapshots when the FS \
                      supports it, copy fallback otherwise). Diff and rollback \
                      supported. Quotas enforced per namespace.",
        primitive: crate::checkpoint::run,
        commands: &[
            "create",
            "diff",
            "rollback",
            "list",
            "status",
            "quota-set",
            "quota-status",
            "namespaces",
        ],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_service",
        description: "Manage cos-managed long-running services: start, stop, \
                      restart, status, health, list, logs, register. Use \
                      stop-all to halt every running service.",
        primitive: crate::service::run,
        commands: &[
            "start", "stop", "stop-all", "restart", "status", "health", "list", "logs", "register",
        ],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_trace",
        description: "Structured trace journal: start/end traces, open/close \
                      spans, show one trace, list recent. Used for auditing \
                      what the agent did and why.",
        primitive: crate::trace::run,
        commands: &["start", "end", "span", "span-end", "show", "list"],
        parallel_safe: false, // start/end/span mutate journal.
    },
    PrimitiveSpec {
        name: "cos_watch",
        description: "Watch files, directories, or processes for changes. \
                      Supports multi-target watches and history queries.",
        primitive: crate::watch::run,
        commands: &["file", "dir", "proc", "on", "multi", "history"],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_ipc",
        description: "Send/receive on cos's local IPC bus (Unix sockets / named \
                      pipes on Windows). Includes lock/barrier primitives.",
        primitive: crate::ipc::run,
        commands: &[
            "send", "recv", "list", "clear", "lock", "unlock", "locks", "barrier", "pipe",
        ],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_browser",
        description: "Manage the local cos-browser (Obscura) CDP server. \
                      Lifecycle only — actual fetches go via the cos web app.",
        primitive: crate::browser::run,
        commands: &["start", "stop", "restart", "status", "health"],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_netfilter",
        description: "Local firewall rules and rate limits (nftables / pf / wfp \
                      depending on platform). Allow/deny by rule, default \
                      policy, export, per-rule rate limits.",
        primitive: crate::netfilter::run,
        commands: &[
            "add",
            "remove",
            "list",
            "check",
            "reset",
            "default",
            "export",
            "rate-limit",
            "rate-limits",
            "rate-limit-remove",
            "rate-check",
        ],
        parallel_safe: false,
    },
    PrimitiveSpec {
        name: "cos_model",
        description: "Manage local model files (ONNX / GGUF) and the dual-engine \
                      runtime (ort + llama.cpp). User imports model files; \
                      agent can list / inspect / load / infer / bench.",
        primitive: crate::model::run,
        commands: &[
            "list", "import", "load", "unload", "infer", "status", "bench", "rm",
        ],
        parallel_safe: false, // import/load/unload mutate engine state.
    },
    PrimitiveSpec {
        name: "cos_doctor",
        description: "Holistic self-check of the agent stack — provider \
                      configuration, engines linked, memory/audit/run-log \
                      health, recent token usage, hook + skill registration. \
                      The `command` argument is ignored; flags drive behaviour. \
                      Flags: --quick (skip log scans + network probe), \
                      --probe-network (one-shot live ping to active provider), \
                      --probe-timeout <secs> (default 30). Output always JSON \
                      with a top-level status of ok | warn | fail.",
        primitive: crate::agent::doctor_cli::doctor_primitive,
        commands: &["run"],
        // Diagnostics-only; no writes outside ephemeral status fields.
        parallel_safe: true,
    },
];

/// Register every cos primitive proxy on the supplied registry, plus the
/// `cos_memory` notes tool. Does NOT register `cos_recall` — the caller must
/// supply a `MemoryDb` via [`register_recall`]. Keeps the proxy registration
/// pure (no IO during test setup).
pub fn register_all(registry: &mut ToolRegistry) {
    for spec in PRIMITIVES {
        let tool = if spec.parallel_safe {
            CosPrimitiveTool::new_readonly(
                spec.name,
                spec.description,
                spec.primitive,
                spec.commands,
            )
        } else {
            CosPrimitiveTool::new(spec.name, spec.description, spec.primitive, spec.commands)
        };
        registry.register(Arc::new(tool));
    }
    registry.register(Arc::new(memory::CosMemoryTool::new()));
}

/// Register the `cos_recall` history-search tool against an explicit DB.
/// Production callers pass a default-path DB; tests pass an in-memory DB.
pub fn register_recall(
    registry: &mut ToolRegistry,
    db: crate::agent::memory::sqlite_fts::MemoryDb,
) {
    registry.register(Arc::new(recall::CosRecallTool::new(db)));
}

/// Register the `cos_recall_semantic` similarity-search tool against
/// an explicit semantic store. The runtime opens the default-path
/// store (when `[embed]` is configured) and passes it in; tests use
/// an in-memory store.
pub fn register_recall_semantic(
    registry: &mut ToolRegistry,
    store: std::sync::Arc<crate::agent::memory::semantic::SemanticStore>,
) {
    registry.register(Arc::new(recall_semantic::CosRecallSemanticTool::new(store)));
}

/// Number of cos primitive tools shipped, *not* counting the higher-level
/// tools (cos_memory etc.). Useful for tests that want to know specifically
/// how many primitives were wired.
pub const fn count() -> usize {
    PRIMITIVES.len()
}

/// Total number of cos_proxy tools registered by `register_all` (primitives
/// + cos_memory). cos_recall is registered separately by `register_recall`.
pub const fn total_count() -> usize {
    PRIMITIVES.len() + 1 // +1 for cos_memory
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_primitive_has_at_least_one_command() {
        for spec in PRIMITIVES {
            assert!(
                !spec.commands.is_empty(),
                "primitive {} has empty command list",
                spec.name
            );
        }
    }

    #[test]
    fn names_are_unique_and_snake_cased() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for spec in PRIMITIVES {
            assert!(seen.insert(spec.name), "duplicate name {}", spec.name);
            assert!(
                spec.name.starts_with("cos_"),
                "name {} should start with cos_",
                spec.name
            );
            assert!(
                spec.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "name {} not snake_case",
                spec.name
            );
        }
    }

    #[test]
    fn register_all_adds_all_primitives() {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        assert_eq!(r.len(), total_count());
        assert!(r.get("cos_sandbox").is_some());
        assert!(r.get("cos_proc").is_some());
        assert!(r.get("cos_sysinfo").is_some());
        assert!(r.get("cos_memory").is_some());
    }

    #[test]
    fn schema_includes_command_enum() {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        let tool = r.get("cos_sandbox").unwrap();
        let schema = tool.input_schema();
        let enum_vals = schema
            .pointer("/properties/command/enum")
            .and_then(Value::as_array)
            .expect("enum must be present");
        assert!(enum_vals.iter().any(|v| v.as_str() == Some("exec")));
    }

    #[tokio::test]
    async fn unknown_command_is_returned_as_tool_error() {
        let tool = CosPrimitiveTool::new(
            "cos_sandbox",
            "test",
            crate::sandbox::run,
            &["exec"],
        );
        let result = tool
            .exec(json!({ "command": "definitely-not-a-command", "args": [] }))
            .await;
        assert!(result.is_error, "expected is_error=true, got {result:?}");
    }

    #[tokio::test]
    async fn missing_command_field_is_returned_as_tool_error() {
        let tool = CosPrimitiveTool::new("cos_sandbox", "test", crate::sandbox::run, &["exec"]);
        let result = tool.exec(json!({ "args": ["whatever"] })).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn args_default_to_empty() {
        // sysinfo "info" works with zero args on every platform.
        let _perms = crate::test_env::PermissiveModeGuard::new();
        let tool = CosPrimitiveTool::new("cos_sysinfo", "test", crate::sysinfo::run, &["info"]);
        let result = tool.exec(json!({ "command": "info" })).await;
        assert!(
            !result.is_error,
            "sysinfo info unexpectedly failed: {}",
            result.content
        );
    }

    #[test]
    fn new_default_is_not_parallel_safe() {
        let t = CosPrimitiveTool::new("cos_x", "desc", crate::sysinfo::run, &["info"]);
        assert!(!t.parallel_safe(), "new() must default to serial");
    }

    #[test]
    fn new_readonly_opts_into_parallel_safe() {
        let t = CosPrimitiveTool::new_readonly("cos_x", "desc", crate::sysinfo::run, &["info"]);
        assert!(t.parallel_safe(), "new_readonly() must opt into parallel");
    }

    #[test]
    fn registered_sysinfo_is_parallel_safe() {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        assert!(
            r.is_parallel_safe("cos_sysinfo"),
            "cos_sysinfo (read-only telemetry) should opt into parallel dispatch"
        );
        assert!(
            !r.is_parallel_safe("cos_sandbox"),
            "cos_sandbox (arbitrary command exec) must stay serial"
        );
    }
}

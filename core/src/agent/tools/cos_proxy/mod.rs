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
}

/// Tool descriptor — name, human description, primitive entry point, and the
/// list of commands the primitive understands. Keep this list in sync with
/// each primitive's `run` `match` arms.
struct PrimitiveSpec {
    name: &'static str,
    description: &'static str,
    primitive: PrimitiveFn,
    commands: &'static [&'static str],
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
    },
    PrimitiveSpec {
        name: "cos_sysinfo",
        description: "Read system telemetry: info, env, resources (cpu/mem), \
                      uptime, proc, mounts, net, cgroup.",
        primitive: crate::sysinfo::run,
        commands: &[
            "info",
            "env",
            "resources",
            "uptime",
            "proc",
            "mounts",
            "net",
            "cgroup",
        ],
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
    },
    PrimitiveSpec {
        name: "cos_cron",
        description: "Schedule recurring jobs. Persists across reboots via cos \
                      service. Cron expression syntax.",
        primitive: crate::cron::run,
        commands: &[
            "add", "remove", "list", "status", "enable", "disable", "logs", "run", "tick",
        ],
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
    },
    PrimitiveSpec {
        name: "cos_trace",
        description: "Structured trace journal: start/end traces, open/close \
                      spans, show one trace, list recent. Used for auditing \
                      what the agent did and why.",
        primitive: crate::trace::run,
        commands: &["start", "end", "span", "span-end", "show", "list"],
    },
    PrimitiveSpec {
        name: "cos_watch",
        description: "Watch files, directories, or processes for changes. \
                      Supports multi-target watches and history queries.",
        primitive: crate::watch::run,
        commands: &["file", "dir", "proc", "on", "multi", "history"],
    },
    PrimitiveSpec {
        name: "cos_ipc",
        description: "Send/receive on cos's local IPC bus (Unix sockets / named \
                      pipes on Windows). Includes lock/barrier primitives.",
        primitive: crate::ipc::run,
        commands: &[
            "send", "recv", "list", "clear", "lock", "unlock", "locks", "barrier", "pipe",
        ],
    },
    PrimitiveSpec {
        name: "cos_browser",
        description: "Manage the local cos-browser (Obscura) CDP server. \
                      Lifecycle only — actual fetches go via the cos web app.",
        primitive: crate::browser::run,
        commands: &["start", "stop", "restart", "status", "health"],
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
    },
];

/// Register every cos primitive proxy on the supplied registry, plus the
/// `cos_memory` notes tool. Does NOT register `cos_recall` — the caller must
/// supply a `MemoryDb` via [`register_recall`]. Keeps the proxy registration
/// pure (no IO during test setup).
pub fn register_all(registry: &mut ToolRegistry) {
    for spec in PRIMITIVES {
        registry.register(Arc::new(CosPrimitiveTool::new(
            spec.name,
            spec.description,
            spec.primitive,
            spec.commands,
        )));
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
}

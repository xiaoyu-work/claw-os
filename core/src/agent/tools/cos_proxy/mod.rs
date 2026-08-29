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
//! Proxy names are not an authorization boundary: one proxy may expose
//! both read and write commands. Inputs are shape-checked here. A proxy
//! bypasses the legacy name prompt only after every command derives and
//! enforces an exact capability before side effects; mixed or incomplete
//! proxies remain on `dangerous_tools`.

pub mod app_memory;
pub mod memory;
pub mod oauth_login;
pub mod recall;
pub mod recall_semantic;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};
use crate::agent::runtime::approval::ApprovalBoundary;

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
    approval_boundary: ApprovalBoundary,
}

impl CosPrimitiveTool {
    /// Construct a serial primitive. It remains on the legacy
    /// tool-name approval path until registration explicitly marks its
    /// command mapping as capability-aware.
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
            approval_boundary: ApprovalBoundary::ToolName,
        }
    }

    /// Variant constructor for primitives whose every command is
    /// read-only. The dispatch loop may run these concurrently with
    /// other parallel-safe calls.
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
            approval_boundary: ApprovalBoundary::ToolName,
        }
    }

    pub(crate) const fn with_capability_approval(mut self) -> Self {
        self.approval_boundary = ApprovalBoundary::Capability;
        self
    }
}

#[async_trait]
impl Tool for CosPrimitiveTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
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
            Some(s) if self.commands.contains(&s) => s.to_string(),
            Some(s) if !s.is_empty() => {
                return ToolResult::err(format!(
                    "unknown command '{s}'. valid commands for {}: {:?}",
                    self.name, self.commands
                ));
            }
            _ => {
                return ToolResult::err(format!(
                    "missing 'command' field. valid commands for {}: {:?}",
                    self.name, self.commands
                ));
            }
        };

        let args: Vec<String> = match input.get("args") {
            None => Vec::new(),
            Some(Value::Array(values)) => {
                let mut args = Vec::with_capacity(values.len());
                for (index, value) in values.iter().enumerate() {
                    let Some(value) = value.as_str() else {
                        return ToolResult::err(format!(
                            "args[{index}] for {} must be a string",
                            self.name
                        ));
                    };
                    args.push(value.to_string());
                }
                args
            }
            Some(_) => {
                return ToolResult::err(format!("'args' for {} must be an array", self.name));
            }
        };

        // cos primitives are sync and may do file IO / spawn processes.
        // A clawd-routed job carries Tokio task-local user/config overrides;
        // spawn_blocking does not inherit those. Keep routed calls on the
        // current task so credential, memory and config lookups cannot fall
        // back to root. Unrouted CLI calls still use the blocking pool.
        let primitive = self.primitive;
        if crate::paths::is_routed_job()
            || crate::paths::current_owner_uid_override().is_some()
            || crate::paths::current_home_override().is_some()
        {
            return match tokio::task::block_in_place(|| primitive(&command, &args)) {
                Ok(value) => ToolResult::ok(
                    serde_json::to_string(&value).unwrap_or_else(|_| value.to_string()),
                ),
                Err(message) => ToolResult::err(message),
            };
        }
        let local_execution = crate::approvals::capture_local_execution();
        let join = tokio::task::spawn_blocking(move || {
            crate::approvals::with_captured_local_execution(local_execution, || {
                primitive(&command, &args)
            })
        })
        .await;

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

    fn approval_boundary(&self) -> ApprovalBoundary {
        self.approval_boundary
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
        description: "Run commands inside a fail-closed bubblewrap+cgroup sandbox \
                      (network denied and root/workspace read-only by default; mandatory \
                      mem/cpu/pids/timeout/output limits and seccomp). Use for any user-supplied or \
                      model-generated shell command.",
        primitive: crate::sandbox::run,
        commands: &["exec"],
        parallel_safe: false, // runs arbitrary commands; never parallel.
    },
    PrimitiveSpec {
        name: "cos_proc",
        description: "Manage long-running processes registered with cos. Spawn accepts only \
                      validated static native Linux executables; use cos_sandbox for shells, \
                      scripts, language runtimes, or dynamically linked programs. Other commands \
                      query status/output, kill/signal, list, wait, renice, stats, and result.",
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
                      identity: info (host distribution + Claw Agent layer) | env | uptime | who | desktop;\n\
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
        description: "Manage the encrypted local cos credential store on any \
                      Linux host, including Ubuntu. Commands store/load/revoke/list \
                      manage secrets; oauth-refresh refreshes an existing Google or \
                      Microsoft login. Initial interactive authorization uses the \
                      dedicated cos_oauth_login tool so tokens stay outside model \
                      context. Never ask users to paste secrets or tokens into chat.",
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
    PrimitiveSpec {
        name: "cos_diagnose",
        description: "Deterministic system diagnosis orchestrator. Routes a symptom \
                      to bounded read-only probes, collects structured evidence, \
                      applies explicit thresholds, and returns confidence-linked \
                      findings plus recommended next actions. Arguments are the \
                      symptom text plus optional --quick, --domain <domain>, and \
                      --path <path>.",
        primitive: crate::agent::diagnose::diagnose_primitive,
        commands: &["run"],
        parallel_safe: true,
    },
];

/// Register every cos primitive proxy on the supplied registry, plus the
/// dedicated OAuth-login and `cos_memory` tools. Does NOT register
/// `cos_recall` — the caller must supply a `MemoryDb` via [`register_recall`].
/// Keeps the proxy registration pure (no IO during test setup).
pub fn register_all(registry: &mut ToolRegistry) {
    for spec in PRIMITIVES {
        let mut tool = if spec.parallel_safe {
            CosPrimitiveTool::new_readonly(
                spec.name,
                spec.description,
                spec.primitive,
                spec.commands,
            )
        } else {
            CosPrimitiveTool::new(spec.name, spec.description, spec.primitive, spec.commands)
        };
        // Only proxies whose complete command surface has an exact
        // validated capability mapping may bypass the legacy
        // `dangerous_tools` prompt.
        if spec.name == "cos_credential" {
            tool = tool.with_capability_approval();
        }
        registry.register(Arc::new(tool));
    }
    registry.register(Arc::new(oauth_login::CosOauthLoginTool::new()));
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

/// Register the `cos_app_memory` tool, which exposes app-pushed memory
/// rows (calendar events, sent emails, document summaries, ...) to the
/// LLM as a dedicated query surface. Backed by the same `MemoryDb` as
/// `cos_recall`, but with source/kind filtering and structured rows.
pub fn register_app_memory(
    registry: &mut ToolRegistry,
    db: crate::agent::memory::sqlite_fts::MemoryDb,
) {
    registry.register(Arc::new(app_memory::CosAppMemoryTool::new(db)));
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
/// + OAuth login + cos_memory). cos_recall is registered separately by
/// `register_recall`.
pub const fn total_count() -> usize {
    PRIMITIVES.len() + 2
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy.rs"
    ));
}

//! cos *apps* proxy tools — bridge cos's Python apps into the
//! agent's tool registry.
//!
//! Each cos app (`fs`, `log`, `notify`, `kv`, `db`, `email`,
//! `calendar`, `search`, `web`) ships its own `main.py` with a
//! `run(command, args)` entry point; the kernel calls them via
//! `bridge::run_python_app`. This module reuses the same bridge
//! so the agent inherits the apps without re-implementing them.
//!
//! Naming: the LLM-facing tool is `cos_app_<name>` (e.g.
//! `cos_app_fs`) so the namespace stays distinct from the
//! `cos_<primitive>` proxies that wrap built-in Rust kernel
//! primitives. Apps and primitives are dispatched through
//! different code paths and have different policy semantics —
//! the name prefix makes the source obvious.
//!
//! The schema of every app tool is the same as the primitive
//! proxies: `{ command: enum, args: array<string> }` so the
//! invocation grammar matches what the model already knows from
//! `cos_proxy::CosPrimitiveTool`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

/// Tool descriptor for one cos Python app.
struct AppSpec {
    /// Tool name surfaced to the LLM. Always `cos_app_<app>`.
    name: &'static str,
    /// Bare app name (matches the directory under apps/).
    app: &'static str,
    description: &'static str,
    commands: &'static [&'static str],
}

const APPS: &[AppSpec] = &[
    AppSpec {
        name: "cos_app_fs",
        app: "fs",
        description: "Agent-native file system. Read/write files with metadata sidecars, \
                      list directories, search content, tag files, query recently changed.",
        commands: &[
            "ls", "read", "write", "rm", "mkdir", "stat", "search", "tag", "recent",
        ],
    },
    AppSpec {
        name: "cos_app_log",
        app: "log",
        description: "Structured kernel log. Write entries (info/warn/error), tail recent, \
                      read filtered by app/status, FTS-style search across history.",
        commands: &["write", "read", "tail", "search"],
    },
    AppSpec {
        name: "cos_app_notify",
        app: "notify",
        description: "Send desktop notifications and list recent notification history.",
        commands: &["send", "list"],
    },
    AppSpec {
        name: "cos_app_kv",
        app: "kv",
        description: "Local key-value store. set/get/del/list/dump. Persists across \
                      sessions in cos data dir.",
        commands: &["set", "get", "del", "list", "dump"],
    },
    AppSpec {
        name: "cos_app_db",
        app: "db",
        description: "SQLite databases under cos data dir. Run read-only `query`, \
                      mutating `exec`, list `tables`/`schema`/`databases`.",
        commands: &["query", "exec", "tables", "schema", "databases"],
    },
    AppSpec {
        name: "cos_app_email",
        app: "email",
        description: "Send / search / list / read email via configured IMAP+SMTP \
                      credentials.",
        commands: &["send", "search", "list", "read"],
    },
    AppSpec {
        name: "cos_app_calendar",
        app: "calendar",
        description: "Calendar events: list, today, create, update, delete. Backed by \
                      configured CalDAV / Google Calendar / iCloud credentials.",
        commands: &["list", "today", "create", "update", "delete"],
    },
    AppSpec {
        name: "cos_app_search",
        app: "search",
        description: "Web and image search via configured search providers.",
        commands: &["web", "image"],
    },
    AppSpec {
        name: "cos_app_web",
        app: "web",
        description: "Read / scrape / screenshot / submit forms on web pages via \
                      cos-browser. Use for any HTTP fetch where you need content, \
                      DOM-rendered output, or visual capture.",
        commands: &["read", "scrape", "screenshot", "submit"],
    },
];

pub struct CosAppTool {
    name: &'static str,
    app: &'static str,
    description: &'static str,
    commands: &'static [&'static str],
}

impl CosAppTool {
    pub const fn new(
        name: &'static str,
        app: &'static str,
        description: &'static str,
        commands: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            app,
            description,
            commands,
        }
    }
}

fn apps_root() -> PathBuf {
    PathBuf::from(std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir() -> String {
    std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into())
}

#[async_trait]
impl Tool for CosAppTool {
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
                    "description": format!(
                        "Subcommand on the cos {} app. See cos app {} __schema__ for \
                         per-command parameters.",
                        self.app, self.app
                    ),
                    "enum": self.commands,
                },
                "args": {
                    "type": "array",
                    "description": "Positional / flag args, exactly as you would type \
                                    after `cos app <app> <command>`.",
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

        let app_name = self.app.to_string();
        let app_dir = apps_root().join(&app_name);
        let data = data_dir();
        let apps = apps_root().to_string_lossy().to_string();

        // bridge::run_python_app does process IO; off-load to the
        // blocking pool to avoid stalling the async runtime.
        let join = tokio::task::spawn_blocking(move || {
            crate::bridge::run_python_app(&app_dir, &command, &args, &data, &apps)
        })
        .await;

        match join {
            Ok(Ok(Some(text))) => ToolResult::ok(text),
            Ok(Ok(None)) => ToolResult::ok(String::new()),
            Ok(Err(message)) => ToolResult::err(message),
            Err(join_err) => ToolResult::err(format!("cos app bridge panicked: {join_err}")),
        }
    }
}

/// Register every cos app proxy on the supplied registry.
pub fn register_all(registry: &mut ToolRegistry) {
    for spec in APPS {
        registry.register(Arc::new(CosAppTool::new(
            spec.name,
            spec.app,
            spec.description,
            spec.commands,
        )));
    }
}

/// Number of cos app proxies shipped.
pub const fn count() -> usize {
    APPS.len()
}

/// Tool name prefix: every app proxy starts with this.
pub const NAME_PREFIX: &str = "cos_app_";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_at_least_one_command() {
        for spec in APPS {
            assert!(
                !spec.commands.is_empty(),
                "app {} has empty command list",
                spec.app
            );
        }
    }

    #[test]
    fn names_are_unique_and_prefixed() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for spec in APPS {
            assert!(seen.insert(spec.name), "duplicate name {}", spec.name);
            assert!(
                spec.name.starts_with(NAME_PREFIX),
                "name {} should start with {}",
                spec.name,
                NAME_PREFIX
            );
            assert_eq!(spec.name, format!("{}{}", NAME_PREFIX, spec.app));
        }
    }

    #[test]
    fn register_all_adds_all_apps() {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        assert_eq!(r.len(), count());
        assert!(r.get("cos_app_fs").is_some());
        assert!(r.get("cos_app_log").is_some());
        assert!(r.get("cos_app_notify").is_some());
        assert!(r.get("cos_app_kv").is_some());
        assert!(r.get("cos_app_db").is_some());
        assert!(r.get("cos_app_email").is_some());
        assert!(r.get("cos_app_calendar").is_some());
        assert!(r.get("cos_app_search").is_some());
        assert!(r.get("cos_app_web").is_some());
    }

    #[test]
    fn schema_includes_command_enum() {
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        let tool = r.get("cos_app_fs").unwrap();
        let schema = tool.input_schema();
        let enum_vals = schema
            .pointer("/properties/command/enum")
            .and_then(Value::as_array)
            .expect("enum must be present");
        let names: Vec<&str> = enum_vals.iter().filter_map(|v| v.as_str()).collect();
        for expected in [
            "ls", "read", "write", "rm", "mkdir", "stat", "search", "tag", "recent",
        ] {
            assert!(
                names.contains(&expected),
                "fs schema enum should contain {expected}, got {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn missing_command_field_is_returned_as_tool_error() {
        let tool = CosAppTool::new("cos_app_fs", "fs", "test", &["ls"]);
        let result = tool.exec(json!({ "args": ["whatever"] })).await;
        assert!(result.is_error);
        assert!(result.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn unknown_command_propagates_app_error() {
        // Pick a command the schema says exists; a non-existent app
        // dir will surface as a bridge error so we exercise the
        // error-pass-through path without depending on python in
        // CI.
        let tool = CosAppTool::new("cos_app_fs", "definitely-not-an-app", "test", &["ls"]);
        // Force an apps dir that doesn't contain the app.
        let prev = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", std::env::temp_dir());
        let result = tool.exec(json!({ "command": "ls", "args": [] })).await;
        match prev {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        assert!(result.is_error, "expected error for missing app");
    }

    #[test]
    fn count_constant_matches_table() {
        assert_eq!(count(), APPS.len());
        assert_eq!(count(), 9);
    }

    #[test]
    fn name_prefix_constant() {
        assert_eq!(NAME_PREFIX, "cos_app_");
    }
}

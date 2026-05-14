//! cos *apps* proxy tools — bridge cos's Python apps into the
//! agent's tool registry.
//!
//! Every directory under `$COS_APPS_DIR` (default `/usr/lib/cos/apps`)
//! that holds a valid `app.json` becomes one tool named
//! `cos_app_<id>`. The manifest is the single source of truth: the
//! tool's description is built from `name + summary + operation
//! labels`, and the `command` enum is the list of declared
//! operation keys. **No app list is hardcoded** — installing a new
//! app and restarting the agent is enough to expose it.
//!
//! Naming: the LLM-facing tool is `cos_app_<id>` (e.g.
//! `cos_app_fs`) so the namespace stays distinct from the
//! `cos_<primitive>` proxies that wrap built-in Rust kernel
//! primitives. Apps and primitives are dispatched through
//! different code paths and have different policy semantics —
//! the name prefix makes the source obvious.
//!
//! The schema is the same as the primitive proxies:
//! `{ command: enum, args: array<string> }` so the invocation
//! grammar matches `cos_proxy::CosPrimitiveTool`.
//!
//! For *fully dynamic* discovery (no restart), see also
//! [`CosAppCatalog`] and [`CosAppRun`] at the bottom of this file:
//! they re-scan the apps dir on every call and dispatch by name.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::caps::manifest::Manifest;

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

/// One LLM-visible proxy bound to a single cos app. All fields are
/// `&'static str` to satisfy the [`Tool`] trait; for manifest-built
/// instances, the strings are allocated with `Box::leak` once at
/// registration time and live for the agent process lifetime (a
/// bounded one-shot leak per app).
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

    /// Build a proxy from a discovered manifest. The strings the
    /// [`Tool`] trait exposes outlive the manifest, so we leak them
    /// — see the type doc for why this is safe.
    fn from_manifest(manifest: &Manifest) -> Self {
        let name: &'static str =
            Box::leak(format!("cos_app_{}", manifest.id).into_boxed_str());
        let app: &'static str = Box::leak(manifest.id.clone().into_boxed_str());
        let description: &'static str = Box::leak(build_description(manifest).into_boxed_str());

        let cmds: Vec<&'static str> = manifest
            .operations
            .keys()
            .map(|k| -> &'static str { Box::leak(k.clone().into_boxed_str()) })
            .collect();
        let commands: &'static [&'static str] = Box::leak(cmds.into_boxed_slice());

        Self {
            name,
            app,
            description,
            commands,
        }
    }
}

/// Compose a single-line description for the LLM out of the
/// manifest's `name`, `summary`, and operation `label`s.
///
/// Format: `"<name>. <summary> Verbs: <op1 label>, <op2 label>, …"`.
/// Falls back gracefully when summary or labels are missing.
fn build_description(manifest: &Manifest) -> String {
    let mut out = String::new();
    let name = manifest.name.current().trim();
    if !name.is_empty() {
        out.push_str(name);
        out.push('.');
    }
    let summary = manifest.summary.current().trim();
    if !summary.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(summary);
        if !summary.ends_with('.') && !summary.ends_with('!') && !summary.ends_with('?') {
            out.push('.');
        }
    }
    if !manifest.operations.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        let labels: Vec<String> = manifest
            .operations
            .iter()
            .map(|(verb, op)| {
                let label = op.label.current().trim();
                if label.is_empty() {
                    verb.clone()
                } else {
                    format!("{} ({})", verb, label)
                }
            })
            .collect();
        out.push_str("Verbs: ");
        out.push_str(&labels.join(", "));
        out.push('.');
    }
    if out.is_empty() {
        format!("cos app `{}` (no description provided).", manifest.id)
    } else {
        out
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

        // Coarse capability gate. Each cos app is reached through
        // `agent.invoke` with a Name scope equal to the app name —
        // e.g. holding `agent.invoke:fs` (or `agent.invoke:*`) is the
        // permission to dispatch *any* `cos_app_fs` command. The
        // fine-grained per-arg checks (e.g. `fs.read` on a specific
        // path) happen *inside* the Python app via
        // `apps/_lib/policy.py::require`, where the args have already
        // been parsed. Schema introspection bypasses this gate so
        // tooling / the agent registry can still describe an app it
        // is not allowed to call.
        if command != "__schema__" {
            if let Err(denial) = crate::caps::require(
                crate::caps::Verb::AGENT_INVOKE,
                crate::caps::Scope::name(self.app),
            ) {
                return ToolResult::err(denial.summary());
            }
        }

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

// ---------------------------------------------------------------------------
// Live catalog + generic runner
// ---------------------------------------------------------------------------
//
// The `APPS` table above ships only the apps the kernel author knew about
// at build time. To let the agent discover *any* installed app — including
// third-party packages added after the agent started — we expose two
// extra tools that re-scan `$COS_APPS_DIR` on every call:
//
//   * `cos_app_catalog` — list / search / show installed apps using their
//     manifest. Read-only; bypasses the `agent.invoke` capability gate
//     (just like schema introspection on `CosAppTool`).
//   * `cos_app_run`     — generic dispatch for any verb on any installed
//     app, guarded by `agent.invoke:name=<app>` exactly like the
//     hand-rolled `cos_app_<name>` proxies.
//
// The hand-rolled proxies are kept for the 10 well-known apps because
// they advertise a typed `enum` of valid commands which the model picks
// up more reliably than a free-form `command` string. The two generic
// tools are the long-tail fallback.

pub struct CosAppCatalog;

#[async_trait]
impl Tool for CosAppCatalog {
    fn name(&self) -> &'static str {
        "cos_app_catalog"
    }

    fn description(&self) -> &'static str {
        "Live catalogue of installed Claw OS apps. Re-reads every app's \
         app.json on each call, so apps installed after the agent \
         started are immediately discoverable without restart. \
         `list` enumerates all apps with their one-line summary; \
         `search <query>` filters apps whose id, name, summary, or \
         operation labels contain the query (case-insensitive); \
         `show <app>` returns full manifest detail including every \
         operation's label, summary, arg list, and capability needs. \
         Pair with `cos_app_run` to invoke any verb you discover here."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["list", "search", "show"],
                    "description": "Catalogue action: `list`, `search`, or `show`.",
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "default": [],
                    "description": "For `search`: the query string. \
                                    For `show`: the app id to inspect. \
                                    Ignored for `list`.",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing 'command'; expected list, search, or show"),
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

        let apps_dir = apps_root();
        let apps = match tokio::task::spawn_blocking(move || crate::apps::discover(&apps_dir)).await
        {
            Ok(map) => map,
            Err(join_err) => {
                return ToolResult::err(format!("apps catalogue scan panicked: {join_err}"));
            }
        };

        match command.as_str() {
            "list" => ToolResult::ok(render_catalog_list(&apps)),
            "search" => {
                let query = args.first().cloned().unwrap_or_default();
                if query.trim().is_empty() {
                    return ToolResult::err("`search` requires a non-empty query in args[0]");
                }
                ToolResult::ok(render_catalog_search(&apps, &query))
            }
            "show" => {
                let Some(name) = args.first() else {
                    return ToolResult::err("`show` requires the app id in args[0]");
                };
                match apps.get(name) {
                    Some(app) => ToolResult::ok(render_app_detail(app)),
                    None => ToolResult::err(format!(
                        "no app named `{name}` installed. Try `cos_app_catalog list`."
                    )),
                }
            }
            other => ToolResult::err(format!(
                "unknown catalogue command `{other}`; expected list, search, or show"
            )),
        }
    }
}

fn render_catalog_list(apps: &std::collections::BTreeMap<String, crate::apps::App>) -> String {
    if apps.is_empty() {
        return "(no apps installed)".to_string();
    }
    let id_width = apps.keys().map(|s| s.len()).max().unwrap_or(0);
    let mut out = String::new();
    out.push_str(&format!("{} installed app(s):\n", apps.len()));
    for (id, app) in apps {
        let summary = app.manifest.summary.current();
        let summary = if summary.is_empty() {
            app.manifest.name.current()
        } else {
            summary
        };
        out.push_str(&format!("  {:<width$}  {}\n", id, summary, width = id_width));
    }
    out
}

fn render_catalog_search(
    apps: &std::collections::BTreeMap<String, crate::apps::App>,
    query: &str,
) -> String {
    let needle = query.to_lowercase();
    let mut hits: Vec<&crate::apps::App> = Vec::new();
    for app in apps.values() {
        let m = &app.manifest;
        let mut matched = m.id.to_lowercase().contains(&needle)
            || m.name.current().to_lowercase().contains(&needle)
            || m.summary.current().to_lowercase().contains(&needle);
        if !matched {
            for op in m.operations.values() {
                if op.label.current().to_lowercase().contains(&needle)
                    || op.summary.current().to_lowercase().contains(&needle)
                {
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            hits.push(app);
        }
    }
    if hits.is_empty() {
        return format!("no apps match `{query}`");
    }
    let id_width = hits.iter().map(|a| a.manifest.id.len()).max().unwrap_or(0);
    let mut out = format!("{} match(es) for `{query}`:\n", hits.len());
    for app in hits {
        let summary = app.manifest.summary.current();
        let summary = if summary.is_empty() {
            app.manifest.name.current()
        } else {
            summary
        };
        out.push_str(&format!(
            "  {:<width$}  {}\n",
            app.manifest.id,
            summary,
            width = id_width
        ));
    }
    out
}

fn render_app_detail(app: &crate::apps::App) -> String {
    let m = &app.manifest;
    let mut out = String::new();
    out.push_str(&format!(
        "{} ({} v{})\n",
        m.name.current(),
        m.id,
        m.version
    ));
    let summary = m.summary.current();
    if !summary.is_empty() {
        out.push_str(&format!("Summary: {}\n", summary));
    }
    out.push_str(&format!("Runtime: {:?}\n", m.runtime));
    out.push_str(&format!("Directory: {}\n", app.dir.display()));

    if let Some(ai) = &m.ai {
        out.push_str("AI policy:\n");
        out.push_str(&format!(
            "  budget: {} units / month\n",
            ai.budget.monthly_units
        ));
        if !ai.models.is_empty() {
            out.push_str(&format!("  models: {}\n", ai.models.join(", ")));
        }
        if !ai.origins.is_empty() {
            out.push_str(&format!("  origins: {:?}\n", ai.origins));
        }
        out.push_str(&format!("  safety: {:?}\n", ai.safety));
    }

    if m.operations.is_empty() {
        out.push_str("\n(no operations declared)\n");
        return out;
    }

    out.push_str("\nOperations:\n");
    for (verb, op) in &m.operations {
        out.push_str(&format!("  {} — {}\n", verb, op.label.current()));
        let op_summary = op.summary.current();
        if !op_summary.is_empty() {
            out.push_str(&format!("      {}\n", op_summary));
        }
        if !op.args.is_empty() {
            let parts: Vec<String> = op
                .args
                .iter()
                .map(|a| {
                    let mut s = format!("{}:{:?}", a.name, a.kind);
                    if a.required {
                        s.push('!');
                    }
                    s
                })
                .collect();
            out.push_str(&format!("      args: {}\n", parts.join(", ")));
        }
        if !op.needs.is_empty() {
            let parts: Vec<String> = op
                .needs
                .iter()
                .map(|n| {
                    let scope = match &n.scope {
                        crate::caps::manifest::ScopeBinding::FromArg { arg } => {
                            format!("from-arg({arg})")
                        }
                        crate::caps::manifest::ScopeBinding::Fixed { scope } => scope.to_string(),
                        crate::caps::manifest::ScopeBinding::Wild => "*".to_string(),
                    };
                    format!("{}:{}", n.verb.as_str(), scope)
                })
                .collect();
            out.push_str(&format!("      needs: {}\n", parts.join(", ")));
        }
    }
    out
}

pub struct CosAppRun;

#[async_trait]
impl Tool for CosAppRun {
    fn name(&self) -> &'static str {
        "cos_app_run"
    }

    fn description(&self) -> &'static str {
        "Invoke any verb on any installed Claw OS app. Generic counterpart \
         to the hand-rolled `cos_app_<name>` proxies — use this when the \
         target app does not have a dedicated proxy, or when you want to \
         dispatch dynamically. Discover apps and their verbs via \
         `cos_app_catalog`. Subject to the same coarse capability gate as \
         the typed proxies (`agent.invoke:name=<app>`); fine-grained \
         per-arg checks (`fs.read` on a specific path, etc.) still fire \
         inside the target app. Pass `command=\"__schema__\"` to read the \
         app's per-verb parameter schema without invoking anything."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "Installed app id (matches the directory name under apps/).",
                },
                "command": {
                    "type": "string",
                    "description": "Verb to invoke. Use `__schema__` to introspect.",
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "default": [],
                    "description": "Positional / flag args, exactly as typed after `cos app <app> <command>`.",
                },
            },
            "required": ["app", "command"],
            "additionalProperties": false,
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let app_name = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing 'app' field"),
        };
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing 'command' field"),
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

        if !is_valid_app_id(&app_name) {
            return ToolResult::err(format!(
                "invalid app id `{app_name}`; expected lowercase letters, digits, `_`, `-`"
            ));
        }

        let app_dir = apps_root().join(&app_name);
        if !app_dir.join("app.json").is_file() {
            return ToolResult::err(format!(
                "no app named `{app_name}` installed. Try `cos_app_catalog list`."
            ));
        }

        if command != "__schema__" {
            if let Err(denial) = crate::caps::require(
                crate::caps::Verb::AGENT_INVOKE,
                crate::caps::Scope::name(&app_name),
            ) {
                return ToolResult::err(denial.summary());
            }
        }

        let data = data_dir();
        let apps = apps_root().to_string_lossy().to_string();
        let app_dir_clone = app_dir.clone();
        let cmd = command.clone();
        let join = tokio::task::spawn_blocking(move || {
            crate::bridge::run_python_app(&app_dir_clone, &cmd, &args, &data, &apps)
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

fn is_valid_app_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().map_or(false, |c| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Register one [`CosAppTool`] per app discovered under `$COS_APPS_DIR`,
/// plus the always-on [`CosAppCatalog`] / [`CosAppRun`] generics.
///
/// Discovery happens *here*, at registry construction time, so the
/// set of typed `cos_app_<id>` tools reflects what was on disk when
/// the agent started. For apps installed after that point — and for
/// long-tail apps the LLM might want to introspect — the dynamic
/// `cos_app_catalog` / `cos_app_run` pair re-scans on every call.
pub fn register_all(registry: &mut ToolRegistry) {
    let apps = crate::apps::discover(&apps_root());
    for app in apps.values() {
        registry.register(Arc::new(CosAppTool::from_manifest(&app.manifest)));
    }
    registry.register(Arc::new(CosAppCatalog));
    registry.register(Arc::new(CosAppRun));
}

/// Tool name prefix: every app proxy starts with this.
pub const NAME_PREFIX: &str = "cos_app_";

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small tempdir holding two synthetic apps so the
    /// dynamic registration path can be exercised without depending
    /// on what's installed on the host.
    fn write_two_demo_apps() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let fs_dir = root.path().join("fs");
        std::fs::create_dir_all(&fs_dir).unwrap();
        std::fs::write(
            fs_dir.join("app.json"),
            serde_json::json!({
                "id": "fs",
                "version": "0.1.0",
                "name": {"en": "Files"},
                "summary": {"en": "Agent-native file system."},
                "operations": {
                    "ls":   {"label": {"en": "List files"}, "args": [], "needs": []},
                    "read": {"label": {"en": "Read a file"}, "args": [], "needs": []}
                }
            })
            .to_string(),
        )
        .unwrap();
        let notify_dir = root.path().join("notify");
        std::fs::create_dir_all(&notify_dir).unwrap();
        std::fs::write(
            notify_dir.join("app.json"),
            serde_json::json!({
                "id": "notify",
                "version": "0.1.0",
                "name": {"en": "Notifications"},
                "summary": {"en": "Send desktop notifications."},
                "operations": {
                    "send": {"label": {"en": "Send a notification"}, "args": [], "needs": []}
                }
            })
            .to_string(),
        )
        .unwrap();
        root
    }

    /// Serialised env mutation: several tests set $COS_APPS_DIR in
    /// parallel — share one lock so they don't fight.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn with_apps_dir<R>(tmp: &tempfile::TempDir, f: impl FnOnce() -> R) -> R {
        let prev = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", tmp.path());
        let r = f();
        match prev {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        r
    }

    #[test]
    fn register_all_picks_up_every_manifest_on_disk() {
        let _g = env_lock();
        let tmp = write_two_demo_apps();
        with_apps_dir(&tmp, || {
            let mut r = ToolRegistry::new();
            register_all(&mut r);
            // 2 typed proxies + catalog + run.
            assert_eq!(r.len(), 4);
            assert!(r.get("cos_app_fs").is_some());
            assert!(r.get("cos_app_notify").is_some());
            assert!(r.get("cos_app_catalog").is_some());
            assert!(r.get("cos_app_run").is_some());
        });
    }

    #[test]
    fn register_all_yields_no_typed_proxies_when_apps_dir_empty() {
        let _g = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", tmp.path());
        let mut r = ToolRegistry::new();
        register_all(&mut r);
        match prev {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        // Only catalog + run survive when no manifests exist.
        assert_eq!(r.len(), 2);
        assert!(r.get("cos_app_catalog").is_some());
        assert!(r.get("cos_app_run").is_some());
    }

    #[test]
    fn manifest_drives_command_enum() {
        let _g = env_lock();
        let tmp = write_two_demo_apps();
        with_apps_dir(&tmp, || {
            let mut r = ToolRegistry::new();
            register_all(&mut r);
            let tool = r.get("cos_app_fs").expect("fs must be registered");
            let schema = tool.input_schema();
            let enum_vals = schema
                .pointer("/properties/command/enum")
                .and_then(Value::as_array)
                .expect("enum must be present");
            let names: std::collections::HashSet<&str> =
                enum_vals.iter().filter_map(|v| v.as_str()).collect();
            assert!(names.contains("ls"), "got {names:?}");
            assert!(names.contains("read"), "got {names:?}");
            assert_eq!(names.len(), 2, "extra commands appeared: {names:?}");
        });
    }

    #[test]
    fn description_includes_name_summary_and_verb_labels() {
        let _g = env_lock();
        let tmp = write_two_demo_apps();
        with_apps_dir(&tmp, || {
            let mut r = ToolRegistry::new();
            register_all(&mut r);
            let tool = r.get("cos_app_fs").unwrap();
            let desc = tool.description();
            assert!(desc.contains("Files"), "want app name in description: {desc}");
            assert!(
                desc.contains("Agent-native file system"),
                "want summary in description: {desc}"
            );
            assert!(
                desc.contains("List files") && desc.contains("Read a file"),
                "want verb labels in description: {desc}"
            );
        });
    }

    #[test]
    fn registered_tool_names_are_unique_and_prefixed() {
        let _g = env_lock();
        let tmp = write_two_demo_apps();
        with_apps_dir(&tmp, || {
            let mut r = ToolRegistry::new();
            register_all(&mut r);
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for name in r.names() {
                if let Some(rest) = name.strip_prefix(NAME_PREFIX) {
                    assert!(seen.insert(name), "duplicate {name}");
                    assert!(!rest.is_empty(), "tool {name} has empty suffix");
                }
            }
            assert_eq!(NAME_PREFIX, "cos_app_");
        });
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

    #[tokio::test]
    async fn strict_mode_without_session_denies_invocation() {
        // With strict perms and no session, the capability gate must
        // refuse before we ever reach the bridge. Other tests set
        // env in parallel; we serialise via a process-wide lock.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let prev_mode = std::env::var("COS_PERMS_MODE").ok();
        let prev_session = std::env::var("COS_SESSION").ok();
        std::env::set_var("COS_PERMS_MODE", "strict");
        std::env::remove_var("COS_SESSION");

        let tool = CosAppTool::new("cos_app_fs", "fs", "test", &["ls"]);
        let result = tool.exec(json!({ "command": "ls", "args": [] })).await;

        match prev_mode {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
        if let Some(v) = prev_session {
            std::env::set_var("COS_SESSION", v);
        }

        assert!(result.is_error, "expected denial in strict mode");
        // Summary always names the verb that was denied.
        assert!(
            result.content.contains("agent.invoke"),
            "denial summary should mention the verb, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn schema_introspection_bypasses_capability_gate() {
        // `__schema__` is the introspection escape hatch — the agent
        // registry must be able to describe an app it cannot run.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let prev_mode = std::env::var("COS_PERMS_MODE").ok();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_PERMS_MODE", "strict");
        // Force a missing app dir so the bridge errors rather than
        // launching python — we only care that we got *past* the gate.
        std::env::set_var("COS_APPS_DIR", std::env::temp_dir());

        let tool = CosAppTool::new("cos_app_fs", "fs", "test", &["ls"]);
        let result = tool.exec(json!({ "command": "__schema__", "args": [] })).await;

        match prev_mode {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        // The bridge will error (no python app installed under temp),
        // but the error must NOT be a capability denial — the schema
        // path is supposed to skip the gate entirely.
        if result.is_error {
            assert!(
                !result.content.contains("agent.invoke"),
                "__schema__ should bypass the capability gate, got: {}",
                result.content
            );
        }
    }

    #[test]
    fn name_prefix_constant() {
        assert_eq!(NAME_PREFIX, "cos_app_");
    }

    // ----- catalog + run --------------------------------------------------

    fn write_demo_apps_dir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let demo_dir = root.path().join("demo");
        std::fs::create_dir_all(&demo_dir).unwrap();
        let manifest = serde_json::json!({
            "id": "demo",
            "version": "0.1.0",
            "name": {"en": "Demo App"},
            "summary": {"en": "Toy app used by catalog tests."},
            "operations": {
                "ping": {
                    "label": {"en": "Ping"},
                    "summary": {"en": "Echo a fixed reply."},
                    "args": [],
                    "needs": []
                }
            }
        });
        std::fs::write(demo_dir.join("app.json"), manifest.to_string()).unwrap();
        root
    }

    #[tokio::test]
    async fn catalog_list_includes_installed_apps() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let tmp = write_demo_apps_dir();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", tmp.path());

        let tool = CosAppCatalog;
        let result = tool.exec(json!({ "command": "list" })).await;

        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert!(!result.is_error, "catalog list unexpectedly errored: {}", result.content);
        assert!(result.content.contains("demo"), "expected demo app in list, got: {}", result.content);
        assert!(
            result.content.contains("Toy app"),
            "summary should appear, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn catalog_search_matches_on_label() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let tmp = write_demo_apps_dir();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", tmp.path());

        let tool = CosAppCatalog;
        let hit = tool.exec(json!({ "command": "search", "args": ["ping"] })).await;
        let miss = tool.exec(json!({ "command": "search", "args": ["zzzz_no_match"] })).await;

        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert!(!hit.is_error);
        assert!(hit.content.contains("demo"), "expected hit on label 'Ping'");
        assert!(!miss.is_error);
        assert!(miss.content.contains("no apps match"));
    }

    #[tokio::test]
    async fn catalog_show_dumps_operation_detail() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let tmp = write_demo_apps_dir();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", tmp.path());

        let tool = CosAppCatalog;
        let result = tool.exec(json!({ "command": "show", "args": ["demo"] })).await;
        let missing = tool.exec(json!({ "command": "show", "args": ["ghost"] })).await;

        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert!(!result.is_error, "show errored: {}", result.content);
        assert!(result.content.contains("ping"));
        assert!(result.content.contains("Ping"));
        assert!(missing.is_error);
    }

    #[tokio::test]
    async fn catalog_bypasses_capability_gate() {
        // Catalog must work in strict mode without a session, because
        // it's a read-only manifest inspection — the entire point is
        // for the agent to discover what *could* be invoked.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let tmp = write_demo_apps_dir();
        let prev_mode = std::env::var("COS_PERMS_MODE").ok();
        let prev_session = std::env::var("COS_SESSION").ok();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_PERMS_MODE", "strict");
        std::env::remove_var("COS_SESSION");
        std::env::set_var("COS_APPS_DIR", tmp.path());

        let tool = CosAppCatalog;
        let result = tool.exec(json!({ "command": "list" })).await;

        match prev_mode {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
        if let Some(v) = prev_session {
            std::env::set_var("COS_SESSION", v);
        }
        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert!(!result.is_error, "catalog must bypass caps; got: {}", result.content);
    }

    #[tokio::test]
    async fn run_rejects_unknown_app() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", std::env::temp_dir());

        let tool = CosAppRun;
        let result = tool
            .exec(json!({ "app": "definitely-not-installed-xyz", "command": "ls" }))
            .await;

        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert!(result.is_error);
        assert!(
            result.content.contains("no app named")
                || result.content.contains("definitely-not-installed-xyz"),
            "expected unknown-app error, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn run_rejects_invalid_app_id() {
        let tool = CosAppRun;
        let result = tool
            .exec(json!({ "app": "Bad/App!", "command": "ls" }))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid app id"));
    }

    #[tokio::test]
    async fn run_requires_app_and_command_fields() {
        let tool = CosAppRun;
        let missing_app = tool.exec(json!({ "command": "ls" })).await;
        let missing_cmd = tool.exec(json!({ "app": "fs" })).await;
        assert!(missing_app.is_error);
        assert!(missing_cmd.is_error);
        assert!(missing_app.content.contains("missing 'app'"));
        assert!(missing_cmd.content.contains("missing 'command'"));
    }

    #[test]
    fn is_valid_app_id_accepts_canonical_and_rejects_garbage() {
        assert!(is_valid_app_id("fs"));
        assert!(is_valid_app_id("a"));
        assert!(is_valid_app_id("my-app_2"));
        assert!(!is_valid_app_id(""));
        assert!(!is_valid_app_id("0name"));
        assert!(!is_valid_app_id("Cap"));
        assert!(!is_valid_app_id("with space"));
        assert!(!is_valid_app_id("../etc"));
    }
}

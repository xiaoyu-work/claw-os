//! cos *apps* proxy tools — bridge cos's Python apps into the
//! agent's tool registry.
//!
//! The default registry exposes two progressive-disclosure gateways:
//! [`CosAppCatalog`] discovers installed apps and [`CosAppRun`] invokes one
//! after discovery. This keeps dozens of per-app schemas out of every model
//! request. Typed `cos_app_<id>` proxies remain available through
//! [`register_all`] for explicit compatibility/testing surfaces.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::caps::manifest::Manifest;

use super::exposure::ToolExposure;
use super::progressive::ToolDisclosure;
use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

/// One LLM-visible proxy bound to a single cos app. Manifest-derived
/// metadata is owned by the tool and released with its registry.
pub struct CosAppTool {
    name: String,
    app: String,
    description: String,
    commands: Vec<String>,
}

impl CosAppTool {
    pub fn new(
        name: impl Into<String>,
        app: impl Into<String>,
        description: impl Into<String>,
        commands: &[&str],
    ) -> Self {
        Self {
            name: name.into(),
            app: app.into(),
            description: description.into(),
            commands: commands
                .iter()
                .map(|command| (*command).to_owned())
                .collect(),
        }
    }

    fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            name: format!("cos_app_{}", manifest.id),
            app: manifest.id.clone(),
            description: build_description(manifest),
            commands: manifest.operations.keys().cloned().collect(),
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

/// Resolve the apps root directory. Honours `COS_APPS_DIR` for tests
/// and dev installs; defaults to the FHS location `/usr/lib/cos/apps`
/// for production.
///
/// Resolve the apps root directory from the `COS_APPS_DIR` env var,
/// falling back to `/usr/lib/cos/apps`.
///
/// **Not cached**: tests set this env var per-test (each call constructs
/// its own scratch dir), so a process-wide cache breaks isolation.
/// In production `COS_APPS_DIR` is set once at boot and an extra
/// env var read per tool call is sub-microsecond — well below the
/// noise floor of the rest of the tool dispatch path.
fn apps_root() -> PathBuf {
    PathBuf::from(std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir() -> String {
    if crate::paths::is_routed_job()
        || crate::paths::current_owner_uid_override().is_some()
    {
        crate::paths::user_data_dir().to_string_lossy().into_owned()
    } else {
        crate::paths::data_dir().to_string_lossy().into_owned()
    }
}

fn manifest_schema_at(app_dir: &Path) -> Result<String, String> {
    let path = app_dir.join("app.json");
    let body = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let manifest = Manifest::from_json(&body)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    Ok(crate::apps::manifest_schema(&manifest).to_string())
}

#[async_trait]
impl Tool for CosAppTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": format!(
                        "Subcommand on the cos {} app. Use `cos_app_catalog show {}` for \
                         manifest-derived parameter details.",
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_caps([crate::caps::Cap::new(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(&self.app),
        )])
    }

    fn disclosure(&self) -> ToolDisclosure {
        ToolDisclosure::extension(
            "app",
            Some(self.app.clone()),
            None,
            ["app".to_string(), "stateless".to_string()],
        )
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
        // `cos_runtime.policy.require`, where the args have already
        // been parsed. Schema introspection bypasses this gate so
        // tooling / the agent registry can still describe an app it
        // is not allowed to call.
        if command != "__schema__" {
            if let Err(denial) = crate::caps::require(
                crate::caps::Verb::AGENT_INVOKE,
                crate::caps::Scope::name(&self.app),
            ) {
                return ToolResult::err(denial.to_string());
            }
        }

        let app_name = self.app.to_string();
        let app_launch = match crate::apps::find_verified(&apps_root(), &app_name)
            .ok()
            .and_then(|app| {
                app.require_verified()
                    .ok()
                    .and_then(|pkg| crate::bridge::AppLaunch::new(std::sync::Arc::clone(pkg)).ok())
            }) {
            Some(launch) => launch,
            None => {
                return ToolResult::err(format!(
                    "no app named `{app_name}` installed. Try `cos_app_catalog list`."
                ));
            }
        };
        if command == "__schema__" {
            return ToolResult::ok(
                crate::apps::manifest_schema(app_launch.manifest()).to_string(),
            );
        }
        let data = data_dir();
        let apps = apps_root().to_string_lossy().to_string();

        if let Some(host) = crate::extension_host::client::current() {
            return match host.run_app(app_name, command, args).await {
                Ok(Some(text)) => ToolResult::ok(untrusted_app_output(&text)),
                Ok(None) => ToolResult::ok(String::new()),
                Err(message) => ToolResult::err(untrusted_app_output(&message)),
            };
        }
        if crate::paths::is_routed_job() {
            return ToolResult::err(
                "the task extension host is unavailable; refusing to execute App code in claw-agentd",
            );
        }
        if crate::paths::current_owner_uid_override().is_some() {
            return match tokio::task::block_in_place(|| {
                crate::bridge::run_python_app(&app_launch, &command, &args, &data, &apps)
            }) {
                Ok(Some(text)) => ToolResult::ok(text),
                Ok(None) => ToolResult::ok(String::new()),
                Err(message) => ToolResult::err(message),
            };
        }
        let join = tokio::task::spawn_blocking(move || {
            crate::bridge::run_python_app(&app_launch, &command, &args, &data, &apps)
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
// To let the agent discover any installed app — including third-party
// packages added after the agent started — these tools re-scan
// `$COS_APPS_DIR` on every call:
//
//   * `cos_app_catalog` — list / search / show installed apps using their
//     manifest. Read-only; bypasses the `agent.invoke` capability gate
//     (just like schema introspection on `CosAppTool`).
//   * `cos_app_run`     — generic dispatch for any verb on any installed
//     app, guarded by `agent.invoke:name=<app>` exactly like the
//     hand-rolled `cos_app_<name>` proxies.
//
// Typed proxies are opt-in through `register_all`; the production default uses
// only these two gateways so app growth does not grow every request schema.

pub struct CosAppCatalog;

#[async_trait]
impl Tool for CosAppCatalog {
    fn name(&self) -> &str {
        "cos_app_catalog"
    }

    fn description(&self) -> &str {
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_all_verbs([crate::caps::Verb::AGENT_OBSERVE])
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
        let apps = match tokio::task::spawn_blocking(move || crate::apps::discover_verified(&apps_dir)).await
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
                        crate::caps::manifest::ScopeBinding::FromArg { arg, .. } => {
                            format!("from-arg({arg})")
                        }
                        crate::caps::manifest::ScopeBinding::FromArgMap { arg, .. } => {
                            format!("from-arg-map({arg})")
                        }
                        crate::caps::manifest::ScopeBinding::FromArgOrWild {
                            arg,
                            wild_when,
                        } => format!("from-arg-or-wild({arg}, {wild_when})"),
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
    fn name(&self) -> &str {
        "cos_app_run"
    }

    fn description(&self) -> &str {
        "Invoke any verb on any installed Claw OS app. Discover unfamiliar \
         apps and their verbs with `cos_app_catalog`, then pass the app id, \
         command, and CLI-style args here. Subject to the coarse \
         `agent.invoke:name=<app>` capability gate; fine-grained per-arg \
         checks (`fs.read` on a specific path, etc.) still fire inside the \
         target app. Pass `command=\"__schema__\"` to read the app's per-verb \
         parameter schema without invoking anything."
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_all_verbs([crate::caps::Verb::AGENT_INVOKE])
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

        let app_launch = match crate::apps::find_verified(&apps_root(), &app_name)
            .ok()
            .and_then(|app| {
                app.require_verified()
                    .ok()
                    .and_then(|pkg| crate::bridge::AppLaunch::new(std::sync::Arc::clone(pkg)).ok())
            }) {
            Some(launch) => launch,
            None => {
                return ToolResult::err(format!(
                    "no app named `{app_name}` installed. Try `cos_app_catalog list`."
                ));
            }
        };

        if command == "__schema__" {
            return ToolResult::ok(
                crate::apps::manifest_schema(app_launch.manifest()).to_string(),
            );
        }

        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(&app_name),
        ) {
            return ToolResult::err(denial.to_string());
        }

        let data = data_dir();
        let apps = apps_root().to_string_lossy().to_string();
        let launch_clone = app_launch.clone();
        let cmd = command.clone();
        if let Some(host) = crate::extension_host::client::current() {
            return match host.run_app(app_name, command, args).await {
                Ok(Some(text)) => ToolResult::ok(untrusted_app_output(&text)),
                Ok(None) => ToolResult::ok(String::new()),
                Err(message) => ToolResult::err(untrusted_app_output(&message)),
            };
        }
        if crate::paths::is_routed_job() {
            return ToolResult::err(
                "the task extension host is unavailable; refusing to execute App code in claw-agentd",
            );
        }
        if crate::paths::current_owner_uid_override().is_some() {
            return match tokio::task::block_in_place(|| {
                crate::bridge::run_python_app(&launch_clone, &cmd, &args, &data, &apps)
            }) {
                Ok(Some(text)) => ToolResult::ok(text),
                Ok(None) => ToolResult::ok(String::new()),
                Err(message) => ToolResult::err(message),
            };
        }
        let join = tokio::task::spawn_blocking(move || {
            crate::bridge::run_python_app(&launch_clone, &cmd, &args, &data, &apps)
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
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn untrusted_app_output(value: &str) -> String {
    crate::agent::safety::untrusted::wrap_untrusted(
        crate::agent::safety::untrusted::TOOL_RESULT_TAG,
        value,
    )
}

/// Register the compact production-default app surface.
///
/// App count does not affect the model schema: unfamiliar apps are discovered
/// with `cos_app_catalog` and invoked through `cos_app_run`.
pub fn register_default(registry: &mut ToolRegistry) {
    registry.register(Arc::new(CosAppCatalog));
    registry.register(Arc::new(CosAppRun));
}

/// Register one typed [`CosAppTool`] per discovered app plus the generic
/// progressive-disclosure gateways.
///
/// This is retained for explicit compatibility and schema tests. Normal agent
/// construction uses [`register_default`] to avoid paying for every manifest
/// on every provider request.
pub fn register_all(registry: &mut ToolRegistry) {
    let apps = crate::apps::discover_verified(&apps_root());
    for app in apps.values() {
        registry.register(Arc::new(CosAppTool::from_manifest(&app.manifest)));
    }
    register_default(registry);
}

/// Tool name prefix: every app proxy starts with this.
pub const NAME_PREFIX: &str = "cos_app_";

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_apps.rs"
    ));
}

//! App manifest — declarative capability requirements.
//!
//! Every app on Claw OS ships an `app.json` in this shape. Apps speak
//! the same vocabulary as the kernel: each operation lists the
//! [`Cap`](super::cap::Cap)s it needs, and the kernel mediates every
//! invocation through that list. There is no implicit access path; if
//! an op doesn't declare a need, it cannot exercise the corresponding
//! verb.
//!
//! ## JSON shape (informal)
//!
//! ```jsonc
//! {
//!   "id": "fs",
//!   "version": "0.2.0",
//!   "name":    "Files",
//!   "summary": "Browse, read, write, and search files.",
//!   "icon":    "📁",
//!   "runtime": "python",
//!   "entry":   "main.py",
//!
//!   "operations": {
//!     "ls": {
//!       "label":   "List files",
//!       "summary": "Show the names of files inside a folder.",
//!       "args": [
//!         { "name": "path", "kind": "path", "required": true }
//!       ],
//!       "needs": [
//!         {
//!           "verb": "fs.meta",
//!           "scope": { "kind": "from-arg", "arg": "path" },
//!           "why":  "Read directory entries to list files."
//!         }
//!       ]
//!     },
//!
//!     "rm": {
//!       "label": "Delete a file",
//!       "args": [ { "name": "path", "kind": "path", "required": true } ],
//!       "needs": [
//!         {
//!           "verb": "fs.delete",
//!           "scope": { "kind": "from-arg", "arg": "path" },
//!           "why":  "Remove the file you specified."
//!         }
//!       ]
//!     }
//!   }
//! }
//! ```
//!
//! ## Key rules enforced by [`Manifest::validate`]
//!
//! - `id` is `[a-z][a-z0-9_-]*` and matches the directory name (the
//!   caller checks the latter).
//! - Every `name`, `label`, `summary`, and `why` must include at least
//!   an English translation.
//! - Every `verb` must be a recognised [`Verb`] (otherwise serde fails
//!   at parse time).
//! - Every `from-arg` scope must reference an arg the operation
//!   actually declares.
//! - `runtime` ∈ {python, node, shell, binary}.
//! - Authors must declare a scope explicitly. There is no implicit
//!   wildcard; `wild` is a separate variant authors opt into knowingly.
//!
//! ## Session tools (Phase 11)
//!
//! An app may additionally expose a long-lived MCP server through a
//! `session` block. Each tool inside it has the same `args` + `needs`
//! shape as an operation, so capability gating and audit are identical
//! to the one-shot CLI path. The kernel spawns the server (via the
//! runtime, using `Session.entry` or the runtime's default
//! `server.<ext>`), runs the MCP handshake, and registers each tool
//! with the agent's [`ToolRegistry`]. See `docs/app-ai-integration.md`
//! §12.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;

use super::scope::Scope;
use super::verb::Verb;

// ---------------------------------------------------------------------------
// Top-level manifest
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    /// Version of the App contract. MCP-first manifests use version 2.
    /// Legacy manifests omit this field until their migration commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    pub name: LocalizedText,
    #[serde(default)]
    pub summary: LocalizedText,
    #[serde(default)]
    pub icon: Option<String>,

    /// Which interpreter the bridge invokes for this app's entry point.
    #[serde(default)]
    pub runtime: Runtime,

    /// Path to the entry file *relative to the app directory*. If
    /// absent the bridge uses the runtime's default (`main.py`,
    /// `main.js`, `main.sh`, `main`).
    #[serde(default)]
    pub entry: Option<String>,

    /// Operations the app exposes, keyed by command name. The key is
    /// the verb the agent sees; the body describes its inputs and
    /// capability needs.
    #[serde(default)]
    pub operations: BTreeMap<String, Operation>,

    /// AI policy. Required iff any operation declares an `ai.*` need.
    /// Absent means the app cannot exercise any AI verb at all — even
    /// if the user explicitly granted it. Authors describe how much
    /// the app may spend, what prompt origins it accepts, and which
    /// safety profile it expects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiPolicy>,

    /// Optional MCP server the app exposes for stateful, agent-driven
    /// tool calls. Absent means the app is one-shot only (the agent can
    /// still call its operations through `cos_app_<id>`). See
    /// [`Session`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Session>,

    /// The single MCP-first App service contract. New Apps declare this
    /// instead of separate one-shot `operations` and stateful `session`
    /// surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Session>,

    /// Optional desktop GUI surface. Presence of this block is the
    /// single signal that the app wants a graphical entry: at
    /// `cos app install` the kernel emits a
    /// `/usr/share/applications/com.clawos.<Id>.desktop` launcher whose
    /// `Exec` routes through `cos app <id> <exec>` so the GUI process is
    /// kernel-spawned (identity/audit/consent apply exactly as they do
    /// for the headless path). Absent means the app is CLI/agent-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<Desktop>,

    /// Free-form dependency declarations. Preserved for forward
    /// compatibility — the bridge's package resolver consumes this.
    #[serde(default)]
    pub dependencies: serde_json::Value,
}

// ---------------------------------------------------------------------------
// AI policy
// ---------------------------------------------------------------------------

/// AI policy block: describes the budget envelope and safety
/// constraints under which this app may exercise `ai.*` verbs.
///
/// All fields are required when the block itself is present, but each
/// has a sensible default (see field docs). If `ai` is absent from a
/// manifest, the kernel rejects every `ai.*` need at validation time.
///
/// **Apps do not pick the model.** The OS owns the AI provider; the
/// machine owner configures it once in `/etc/cos/agent.toml`. Every
/// app's call runs through that same provider/model. Apps declare
/// *capability* (which verbs they need), *budget* (how many tokens
/// they may spend), *safety* profile, and *origin* — never which model
/// to talk to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiPolicy {
    /// How much the app may spend in a single billing period.
    pub budget: AiBudget,

    /// Safety profile applied to every call. `strict` and `standard`
    /// both run prompt-secret redaction (the kernel scrubs obvious
    /// API keys / tokens / PEM blocks before the prompt reaches the
    /// provider); `minimal` disables redaction and runs in
    /// audit-only mode. The owner can lower a profile per-call with
    /// `ai.bypass`; the app cannot.
    ///
    /// Reserved tiers — additional pipeline stages (injection
    /// detection, response-side redaction, classifier-driven
    /// human-in-the-loop review) may be added later without
    /// renaming the variants. Apps should declare the tier they
    /// *intend*; the kernel decides what runs inside it.
    #[serde(default)]
    pub safety: AiSafety,

    /// Which prompt origins the app expects to handle. The kernel
    /// rejects calls whose declared `origin` is not in this list.
    /// Defaults to `["trusted"]` — apps must opt in to receiving
    /// external content.
    #[serde(default = "default_origins")]
    pub origins: Vec<PromptOrigin>,

    /// Allowlist of catalog tool names the app may expose to a model
    /// via `cos ai chat --tools`. Each entry must be a known tool in
    /// `crate::ai::tools::CATALOG`; unknown names are rejected at
    /// install time so a typo can never reach the gate.
    ///
    /// The kernel still gates each actual tool call on the underlying
    /// `caps.needs[]` (e.g. `fs.read`) — this list only controls
    /// which tools the model is *told about*. An empty list (the
    /// default) means the app cannot request any tools.
    #[serde(default)]
    pub tools: Vec<String>,
}

/// Per-period AI token cap. Zero disables enforcement.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiBudget {
    /// Abstract billing units (1 chat token = 1 unit, 1 image = 1000
    /// units, 1s TTS = 50 units, etc.). The kernel hard-denies any
    /// call whose pre-charge estimate would push usage over the cap.
    #[serde(default)]
    pub monthly_units: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AiSafety {
    /// Prompt-secret redaction enabled. Default for any app
    /// handling external content. Reserved as the home for
    /// stricter pipeline stages added in future releases.
    #[default]
    Strict,
    /// Prompt-secret redaction enabled. Same scrub set as `strict`
    /// today; behaves as a distinct tier so future stricter stages
    /// can land in `strict` without renaming.
    Standard,
    /// Audit-only. Redaction disabled. Reserved for fully trusted
    /// system apps; user must confirm at install time.
    Minimal,
}

/// Where the prompt content originated. Carrying this on every AI
/// call gives the kernel a stable signal for downstream policy
/// decisions (auditing, origin-allowlists in the manifest, and
/// future safety stages that may treat external content more
/// strictly than developer-authored prompts).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PromptOrigin {
    /// Prompt is fully authored by the app developer.
    Trusted,
    /// Prompt contains text the user typed in this session.
    UserInput,
    /// Prompt contains content fetched from outside (email body, web
    /// page, file contents, another agent's output). Apps must opt
    /// in to receiving this origin via `ai.origins[]`.
    ExternalContent,
}

fn default_origins() -> Vec<PromptOrigin> {
    vec![PromptOrigin::Trusted]
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    #[default]
    Python,
    Node,
    Shell,
    Binary,
}

impl Runtime {
    /// Stable lowercase name, matching the manifest wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Runtime::Python => "python",
            Runtime::Node => "node",
            Runtime::Shell => "shell",
            Runtime::Binary => "binary",
        }
    }

    /// Default entry file for this runtime. Platform-aware: Windows
    /// gets `.bat` / `.exe` so packaged apps can ship a single
    /// manifest that works on every OS.
    pub fn default_entry(self) -> &'static str {
        match self {
            Runtime::Python => "main.py",
            Runtime::Node => "main.js",
            Runtime::Shell => {
                if cfg!(windows) {
                    "main.bat"
                } else {
                    "main.sh"
                }
            }
            Runtime::Binary => {
                if cfg!(windows) {
                    "main.exe"
                } else {
                    "main"
                }
            }
        }
    }

    /// Default entry file for an app's long-lived MCP session server.
    /// Lives alongside `default_entry()` (the one-shot CLI entry) so an
    /// app can ship both surfaces without naming them by hand.
    pub fn default_session_entry(self) -> &'static str {
        match self {
            Runtime::Python => "server.py",
            Runtime::Node => "server.js",
            Runtime::Shell => {
                if cfg!(windows) {
                    "server.bat"
                } else {
                    "server.sh"
                }
            }
            Runtime::Binary => {
                if cfg!(windows) {
                    "server.exe"
                } else {
                    "server"
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub label: LocalizedText,
    #[serde(default)]
    pub summary: LocalizedText,
    /// Forward piped caller input to this operation. Interactive stdin is
    /// still closed so agent and desktop launches cannot block on a prompt.
    #[serde(default)]
    pub stdin: bool,
    /// Declared input parameters. Order is significant for the UI.
    #[serde(default)]
    pub args: Vec<Arg>,
    /// Capability requirements. Empty means the operation is purely
    /// local (no gated action); the kernel still records the call in
    /// the audit log but does not prompt for permission.
    #[serde(default)]
    pub needs: Vec<Need>,
}

#[derive(Clone, Debug)]
pub struct EffectiveCall {
    pub values: BTreeMap<String, serde_json::Value>,
    pub needs: Vec<Vec<super::cap::Cap>>,
    pub defaulted: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Arg {
    /// Identifier referenced by `from-arg` scope bindings.
    pub name: String,
    /// What kind of value this arg holds; the UI uses this to pick a
    /// widget and to validate the input.
    pub kind: ArgKind,
    /// How the value is bound on the one-shot CLI. Omitted booleans retain
    /// their historical flag binding; every other omitted binding is
    /// positional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ArgBinding>,
    #[serde(default)]
    pub required: bool,
    /// Make this argument required only when another effective argument
    /// satisfies the declared condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_when: Option<NeedCondition>,
    /// Bind every occurrence in order and expose the value as a JSON array.
    #[serde(default)]
    pub repeatable: bool,
    /// Additional accepted option spellings mapped to this same value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Accept this flag-bound value as a surplus leading positional for
    /// backward-compatible grammars.
    #[serde(default)]
    pub positional_alias: bool,
    /// Optional closed set of accepted scalar values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "deserialize_non_null_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub default: Option<serde_json::Value>,
    /// Derive an omitted value from another declared argument. The bridge
    /// materializes the result before launching the App so capability
    /// derivation and the handler consume the same value.
    #[serde(
        default,
        deserialize_with = "deserialize_non_null_default_from",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_from: Option<ArgDefaultBinding>,
    /// Trusted kernel resolver applied before binding and capability
    /// derivation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_resolver: Option<TrustedArgResolver>,
    /// Human-readable help. Optional.
    #[serde(default)]
    pub label: LocalizedText,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArgDefaultBinding {
    /// Previously declared argument used as the input value.
    pub arg: String,
    /// Optional deterministic transformation applied to the input.
    #[serde(default)]
    pub transform: ArgDefaultTransform,
    /// Literal text prepended after transformation.
    #[serde(default)]
    pub prefix: String,
    /// Replacement used when the transformation produces no safe value.
    #[serde(default)]
    pub fallback: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArgDefaultTransform {
    #[default]
    Identity,
    UrlPathBasename,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TrustedArgResolver {
    EmailProvider,
    EmailHost,
    CalendarProvider,
    NtfyServer,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArgKind {
    /// Filesystem path. Matches [`Scope::Path`] when used in a scope.
    Path,
    /// `host[:port]`. Matches [`Scope::Host`].
    Host,
    /// Arbitrary named resource. Matches [`Scope::Name`].
    Name,
    /// Free-form text — not bindable to a scope.
    Text,
    /// Numeric input.
    Number,
    /// Integer input.
    Integer,
    /// Boolean toggle.
    Bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArgBinding {
    #[default]
    Positional,
    Flag,
}

impl ArgBinding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Positional => "positional",
            Self::Flag => "flag",
        }
    }
}

impl Arg {
    pub fn effective_binding(&self) -> ArgBinding {
        self.binding.unwrap_or_else(|| {
            if self.kind == ArgKind::Bool {
                ArgBinding::Flag
            } else {
                ArgBinding::Positional
            }
        })
    }

    fn accepts_value(&self, value: &serde_json::Value) -> bool {
        if self.repeatable {
            value.as_array().is_some_and(|values| {
                values.iter().all(|value| self.accepts_scalar(value))
            })
        } else {
            self.accepts_scalar(value)
        }
    }

    fn accepts_scalar(&self, value: &serde_json::Value) -> bool {
        self.kind.accepts_default(value)
            && (self.choices.is_empty() || self.choices.contains(value))
    }
}

fn deserialize_non_null_default<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(serde::de::Error::custom(
            "`default` cannot be null; omit it when there is no default",
        ));
    }
    Ok(Some(value))
}

fn deserialize_non_null_default_from<'de, D>(
    deserializer: D,
) -> Result<Option<ArgDefaultBinding>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(serde::de::Error::custom(
            "`default_from` cannot be null; omit it when unused",
        ));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

impl ArgKind {
    /// Returns true if values of this kind can populate a [`Scope`].
    pub fn binds_to_scope(self) -> bool {
        matches!(self, ArgKind::Path | ArgKind::Host | ArgKind::Name)
    }

    fn accepts_default(self, value: &serde_json::Value) -> bool {
        match self {
            ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => value.is_string(),
            ArgKind::Number => value.is_number(),
            ArgKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            ArgKind::Bool => value.is_boolean(),
        }
    }
}

fn apply_arg_defaults(
    declarations: &[Arg],
    values: &mut BTreeMap<String, serde_json::Value>,
) -> Result<Vec<String>, String> {
    let mut applied = Vec::new();
    for declaration in declarations {
        if values.contains_key(&declaration.name) {
            continue;
        }
        let value = if let Some(default) = &declaration.default {
            Some(default.clone())
        } else if let Some(binding) = &declaration.default_from {
            let source = values
                .get(&binding.arg)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "arg `{}` default source `{}` was not supplied",
                        declaration.name, binding.arg
                    )
                })?;
            let transformed = match binding.transform {
                ArgDefaultTransform::Identity => source.to_string(),
                ArgDefaultTransform::UrlPathBasename => safe_url_path_basename(source)
                    .or(binding.fallback.as_deref())
                    .ok_or_else(|| {
                        format!(
                            "arg `{}` could not derive a safe URL path basename from `{}`",
                            declaration.name, binding.arg
                        )
                    })?
                    .to_string(),
            };
            Some(serde_json::Value::String(format!(
                "{}{transformed}",
                binding.prefix
            )))
        } else {
            None
        };
        if let Some(value) = value {
            values.insert(declaration.name.clone(), value);
            applied.push(declaration.name.clone());
        }
    }
    Ok(applied)
}

pub(crate) fn resolve_effective_args(
    declarations: &[Arg],
    supplied: &BTreeMap<String, serde_json::Value>,
    paths: Option<&super::args::PathContext>,
) -> Result<(BTreeMap<String, serde_json::Value>, Vec<String>), String> {
    let mut values = supplied.clone();
    let defaulted = apply_arg_defaults(declarations, &mut values)?;
    if let Some(disallowed) = declarations.iter().find(|declaration| {
        declaration.required_when.as_ref().is_some_and(|condition| {
            !condition_applies(Some(condition), &values)
                && values.contains_key(&declaration.name)
        })
    }) {
        return Err(format!(
            "argument `{}` is only accepted when required_when applies",
            disallowed.name
        ));
    }
    if let Some(required) = declarations.iter().find(|declaration| {
        argument_is_required(declaration, &values)
            && !values.contains_key(&declaration.name)
    })
    {
        return Err(format!("argument `{}` is required", required.name));
    }
    for declaration in declarations {
        if declaration.kind == ArgKind::Bool
            && declaration.required_when.is_none()
            && !values.contains_key(&declaration.name)
        {
            values.insert(declaration.name.clone(), serde_json::Value::Bool(false));
        }
    }
    super::args::validate_bound_args(declarations, &values)?;
    if let Some(paths) = paths {
        super::args::resolve_path_args(declarations, &mut values, paths)?;
    }
    Ok((values, defaulted))
}

pub(crate) fn argument_is_required(
    declaration: &Arg,
    values: &BTreeMap<String, serde_json::Value>,
) -> bool {
    declaration.required
        || declaration
            .required_when
            .as_ref()
            .is_some_and(|condition| condition_applies(Some(condition), values))
}

fn safe_url_path_basename(value: &str) -> Option<&str> {
    let end = value
        .find(['?', '#'])
        .unwrap_or(value.len());
    let without_suffix = &value[..end];
    let path = if let Some(scheme) = without_suffix.find("://") {
        let after_authority = &without_suffix[scheme + 3..];
        after_authority
            .find('/')
            .map(|slash| &after_authority[slash..])
            .unwrap_or("")
    } else {
        without_suffix
    };
    let basename = path.rsplit('/').next().unwrap_or("");
    if basename.is_empty()
        || matches!(basename, "." | "..")
        || basename.contains('\\')
        || basename.chars().any(char::is_control)
    {
        None
    } else {
        Some(basename)
    }
}

// ---------------------------------------------------------------------------
// Session (MCP) — long-lived agent-driven tools
// ---------------------------------------------------------------------------

/// Declares a long-lived MCP server the app launches when an agent
/// session needs cross-call state. The agent attaches to this server
/// through the kernel's MCP bridge; every `tools/call` it makes is
/// caps-gated using the manifest's per-tool `needs[]` and audited the
/// same way `cos ai chat` is.
///
/// Authors who want a stateless one-shot integration should keep using
/// `operations` (the kernel auto-wraps each op as a `cos_app_<id>`
/// agent tool). `Session` is for when the app holds in-memory state
/// across calls or kicks off background work.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    /// Path to the MCP server entry file, relative to the app
    /// directory. If absent, the kernel uses
    /// [`Runtime::default_session_entry`].
    #[serde(default)]
    pub entry: Option<String>,

    /// Wire protocol. Only `stdio` is supported today (matches the
    /// kernel's [`mcp::transport::StdioTransport`]).
    #[serde(default)]
    pub transport: SessionTransport,

    /// How long the App service remains available.
    #[serde(default)]
    pub lifecycle: McpLifecycle,

    /// Which authenticated Claw principals may address this service.
    /// This is a restriction only; callers still need exact invoke
    /// authority and every tool's capabilities.
    #[serde(default)]
    pub access: McpAccess,

    /// Tools the app advertises through this session. The list is
    /// authoritative: the kernel only forwards `tools/call` requests
    /// for names that appear here, and runs the declared `needs[]` as
    /// the cap gate before forwarding.
    #[serde(default)]
    pub tools: Vec<SessionTool>,
}

/// Public MCP-first name for the App service contract. The alias keeps the
/// migration buildable while legacy `session` manifests are converted.
pub type McpService = Session;

/// Wire protocol for [`Session`]. Stdio is the de-facto MCP default
/// and the one our integration already implements.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionTransport {
    #[default]
    Stdio,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum McpLifecycle {
    /// Start on first use and stop after the host's idle deadline.
    #[default]
    Lazy,
    /// Keep one owner-scoped instance running across Agent sessions.
    AlwaysOn,
    /// Keep the service only while its desktop App is running.
    WhileAppRunning,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct McpAccess {
    /// The owner-scoped system Agent may discover and call this service.
    #[serde(default = "default_mcp_system_agent")]
    pub system_agent: bool,
    /// Verified App identities allowed to request this service.
    #[serde(default)]
    pub apps: Vec<String>,
    /// Authenticated agents entering through the external MCP gateway.
    #[serde(default)]
    pub external_agents: bool,
}

impl Default for McpAccess {
    fn default() -> Self {
        Self {
            system_agent: true,
            apps: Vec::new(),
            external_agents: false,
        }
    }
}

fn default_mcp_system_agent() -> bool {
    true
}

/// One MCP-callable tool the app exposes through its [`Session`].
///
/// Mirrors [`Operation`] field-for-field on purpose: `args` + `needs`
/// drive both the agent's view (auto-generated JSON Schema for the
/// model) and the kernel's enforcement (cap resolution at call time).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTool {
    /// Globally unique tool name. Convention: `<app_id>.<verb>`
    /// (e.g. `kv.get`). Must match `[a-z][a-z0-9._-]*` and be unique
    /// within the session.
    pub name: String,

    /// One-line description surfaced to the model and to
    /// `cos app tool list`.
    pub summary: LocalizedText,

    /// Declared input parameters. Same semantics as
    /// [`Operation::args`]: the order is significant for the UI, names
    /// must be unique, and any [`Need::scope`] with
    /// [`ScopeBinding::FromArg`] must reference one of these by name.
    #[serde(default)]
    pub args: Vec<Arg>,

    /// Capability requirements the kernel checks before forwarding the
    /// MCP `tools/call` to the app. Empty means the call is purely
    /// local (kernel still emits an audit row).
    #[serde(default)]
    pub needs: Vec<Need>,
}

/// Public MCP-first name for a manifest-declared service tool.
pub type McpTool = SessionTool;

// ---------------------------------------------------------------------------
// Desktop GUI surface
// ---------------------------------------------------------------------------

/// Desktop GUI surface for an app.
///
/// Declaring this block is the single lever a developer uses to say "I
/// want a graphical entry". It does **not** wrap a UI toolkit — the app
/// draws its own window in whatever toolkit/language it likes ("World
/// A"). The block only drives `.desktop` launcher generation at install
/// time and the kernel's long-lived `--gui` launch path.
///
/// The generated launcher's `Exec` is
/// `cos app <id> <exec> %F`, so the GUI is kernel-spawned and inherits
/// the same `COS_APP_ID` identity, audit, and consent machinery as the
/// headless operation path. A hand-written `.desktop` that ran the app
/// binary directly would bypass that — hence generation is mandatory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Desktop {
    /// Argument the kernel passes to the app entry when launched from
    /// the desktop. Defaults to `--gui`. The app's entry inspects this
    /// (or the `COS_APP_GUI=1` env the bridge sets) to enter its GUI
    /// event loop instead of running a one-shot operation.
    #[serde(default = "default_desktop_exec")]
    pub exec: String,

    /// Display name for the launcher entry. Falls back to the
    /// manifest's top-level `name` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<LocalizedText>,

    /// Icon name (freedesktop icon-theme lookup) or absolute path.
    /// Falls back to the manifest's top-level `icon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Freedesktop menu categories. `ClawOS` is prepended automatically
    /// at generation time, so apps only list their own (`Utility`,
    /// `Development`, ...).
    #[serde(default)]
    pub categories: Vec<String>,

    /// MIME types this app can open, surfaced as file associations in
    /// the generated launcher.
    #[serde(default)]
    pub mime_types: Vec<String>,

    /// Whether the launcher should reuse a running instance instead of
    /// spawning a second process. Surfaced as `SingleMainWindow=true`.
    #[serde(default)]
    pub single_instance: bool,

    /// Whether this GUI is intentionally hosted by `cosmic-panel` and may
    /// inherit the panel's restricted Wayland socket and layout environment.
    /// Leave false for ordinary GUI apps so panel credentials cannot leak
    /// through launches triggered by panel buttons.
    #[serde(default)]
    pub panel_applet: bool,
}

fn default_desktop_exec() -> String {
    "--gui".to_string()
}

// ---------------------------------------------------------------------------
// Capability needs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Need {
    pub verb: Verb,
    pub scope: ScopeBinding,
    /// Explicit condition controlling whether this need applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<NeedCondition>,
    /// Reason shown to the user in the approval dialog. Authors are
    /// expected to write this in plain language ("Read the file you
    /// asked me to summarise"), not jargon.
    pub why: LocalizedText,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NeedCondition {
    ArgPresent { arg: String },
    ArgEquals { arg: String, value: serde_json::Value },
    ArgNotEquals { arg: String, value: serde_json::Value },
}

/// How an operation's scope is determined at invocation time.
///
/// - [`ScopeBinding::FromArg`] — late binding: at call time the kernel
///   reads the named argument's value and constructs a [`Scope`]
///   matching the [`ArgKind`].
/// - [`ScopeBinding::Fixed`] — the scope is hard-coded in the manifest.
///   Useful for ops that always touch the same resource (e.g. a
///   per-app data directory).
/// - [`ScopeBinding::Wild`] — explicit wildcard. The author has to spell
///   this out; there is no implicit `*`.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScopeBinding {
    FromArg {
        arg: String,
        #[serde(default, skip_serializing_if = "ScopeTransform::is_identity")]
        transform: ScopeTransform,
    },
    FromArgMap {
        arg: String,
        values: BTreeMap<String, Scope>,
    },
    FromArgOrWild {
        arg: String,
        wild_when: String,
    },
    Fixed {
        scope: Scope,
    },
    Wild,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeTransform {
    #[default]
    Identity,
    Parent,
    UrlHost,
}

impl ScopeTransform {
    fn is_identity(&self) -> bool {
        *self == Self::Identity
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum NeedConditionWire {
    #[serde(rename = "arg-present")]
    Present { arg: String },
    #[serde(rename = "arg-equals")]
    Equals { arg: String, value: serde_json::Value },
    #[serde(rename = "arg-not-equals")]
    NotEquals { arg: String, value: serde_json::Value },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ScopeBindingWire {
    FromArg {
        arg: String,
        #[serde(default)]
        transform: ScopeTransform,
    },
    FromArgMap {
        arg: String,
        values: BTreeMap<String, Scope>,
    },
    FromArgOrWild {
        arg: String,
        wild_when: String,
    },
    Fixed {
        scope: Scope,
    },
    Wild,
}

fn reject_unknown_tagged_fields<E>(
    value: &serde_json::Value,
    fields_by_kind: &'static [(&'static str, &'static [&'static str])],
) -> Result<(), E>
where
    E: serde::de::Error,
{
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let Some(kind) = object.get("kind").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let Some((_, allowed)) = fields_by_kind
        .iter()
        .find(|(candidate, _)| *candidate == kind)
    else {
        return Ok(());
    };
    if let Some(field) = object
        .keys()
        .find(|field| field.as_str() != "kind" && !allowed.contains(&field.as_str()))
    {
        return Err(E::unknown_field(field, allowed));
    }
    Ok(())
}

impl<'de> Deserialize<'de> for NeedCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        reject_unknown_tagged_fields::<D::Error>(
            &value,
            &[
                ("arg-present", &["arg"]),
                ("arg-equals", &["arg", "value"]),
                ("arg-not-equals", &["arg", "value"]),
            ],
        )?;
        let wire: NeedConditionWire =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(match wire {
            NeedConditionWire::Present { arg } => Self::ArgPresent { arg },
            NeedConditionWire::Equals { arg, value } => Self::ArgEquals { arg, value },
            NeedConditionWire::NotEquals { arg, value } => Self::ArgNotEquals { arg, value },
        })
    }
}

impl<'de> Deserialize<'de> for ScopeBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        reject_unknown_tagged_fields::<D::Error>(
            &value,
            &[
                ("from-arg", &["arg", "transform"]),
                ("from-arg-map", &["arg", "values"]),
                ("from-arg-or-wild", &["arg", "wild_when"]),
                ("fixed", &["scope"]),
                ("wild", &[]),
            ],
        )?;
        let wire: ScopeBindingWire =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(match wire {
            ScopeBindingWire::FromArg { arg, transform } => Self::FromArg { arg, transform },
            ScopeBindingWire::FromArgMap { arg, values } => Self::FromArgMap { arg, values },
            ScopeBindingWire::FromArgOrWild { arg, wild_when } => {
                Self::FromArgOrWild { arg, wild_when }
            }
            ScopeBindingWire::Fixed { scope } => Self::Fixed { scope },
            ScopeBindingWire::Wild => Self::Wild,
        })
    }
}

// ---------------------------------------------------------------------------
// Parsing & validation
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid manifest id `{0}`: must match [a-z][a-z0-9_-]*")]
    InvalidId(String),
    #[error("invalid operation key `{0}`: must match [a-z][a-z0-9_.]*")]
    InvalidOperationKey(String),
    #[error("operation `{op}`: arg `{arg}` declared twice")]
    DuplicateArg { op: String, arg: String },
    #[error("operation `{op}`: arg `{arg}` default is invalid: {detail}")]
    ArgDefaultInvalid {
        op: String,
        arg: String,
        detail: String,
    },
    #[error("operation `{op}`: need #{idx} references undeclared arg `{arg}`")]
    NeedRefsUndeclaredArg { op: String, idx: usize, arg: String },
    #[error(
        "operation `{op}`: need #{idx} (verb `{verb}`) binds to arg `{arg}` of kind \
         `{kind:?}` which cannot populate a scope (expected path/host/name)"
    )]
    NeedArgKindMismatch {
        op: String,
        idx: usize,
        verb: String,
        arg: String,
        kind: ArgKind,
    },
    #[error("operation `{op}`: need #{idx}: {detail}")]
    NeedInvalid {
        op: String,
        idx: usize,
        detail: String,
    },
    #[error("operation `{op}`: {field}: {detail}")]
    LocalizedTextInvalid {
        op: String,
        field: &'static str,
        detail: String,
    },
    #[error("manifest field `{field}`: {detail}")]
    TopLevelTextInvalid { field: &'static str, detail: String },
    #[error(
        "operation `{op}`: need #{idx} (verb `{verb}`) is an AI verb but the manifest \
         has no `ai` block — declare one with budget, safety, and origins"
    )]
    AiNeedMissingPolicy {
        op: String,
        idx: usize,
        verb: String,
    },
    #[error(
        "manifest `ai` block: `origins` list is empty — at least one of \
         `trusted`, `user-input`, `external-content` is required"
    )]
    AiPolicyNoOrigins,
    #[error(
        "manifest field `{field}`: apps cannot request `ai.bypass`; it is \
         reserved for the device owner"
    )]
    AiBypassNotAllowedForApps { field: &'static str },
    #[error(
        "manifest `ai.tools[]`: unknown tool `{name}` — not in the kernel \
         catalog. Run `cos ai tools` to list known tools."
    )]
    AiUnknownTool { name: String },
    #[error("manifest `ai.tools[]`: tool `{name}` declared twice")]
    AiDuplicateTool { name: String },
    #[error("manifest `mcp` requires `schema_version: 2`")]
    McpSchemaVersion,
    #[error("manifest cannot declare both `mcp` and legacy `session`")]
    McpLegacySessionConflict,
    #[error("manifest `mcp.access.apps[]`: invalid App id `{app}`")]
    McpAccessInvalidApp { app: String },
    #[error("manifest `mcp.access.apps[]`: App `{app}` declared twice")]
    McpAccessDuplicateApp { app: String },
    #[error("session tool `{tool}`: name must match [a-z][a-z0-9._-]* — got `{tool}`")]
    SessionToolInvalidName { tool: String },
    #[error("session tool `{name}` declared twice")]
    SessionDuplicateTool { name: String },
    #[error("session tool `{tool}`: arg `{arg}` declared twice")]
    SessionDuplicateArg { tool: String, arg: String },
    #[error("session tool `{tool}`: arg `{arg}` default is invalid: {detail}")]
    SessionArgDefaultInvalid {
        tool: String,
        arg: String,
        detail: String,
    },
    #[error("session tool `{tool}`: need #{idx} references undeclared arg `{arg}`")]
    SessionNeedRefsUndeclaredArg {
        tool: String,
        idx: usize,
        arg: String,
    },
    #[error(
        "session tool `{tool}`: need #{idx} (verb `{verb}`) binds to arg `{arg}` of \
         kind `{kind:?}` which cannot populate a scope (expected path/host/name)"
    )]
    SessionNeedArgKindMismatch {
        tool: String,
        idx: usize,
        verb: String,
        arg: String,
        kind: ArgKind,
    },
    #[error("session tool `{tool}`: need #{idx}: {detail}")]
    SessionNeedInvalid {
        tool: String,
        idx: usize,
        detail: String,
    },
    #[error("session tool `{tool}`: {field}: {detail}")]
    SessionLocalizedTextInvalid {
        tool: String,
        field: &'static str,
        detail: String,
    },
    #[error(
        "session tool `{tool}`: need #{idx} (verb `{verb}`) is an AI verb but the \
         manifest has no `ai` block — declare one with budget, safety, and origins"
    )]
    SessionAiNeedMissingPolicy {
        tool: String,
        idx: usize,
        verb: String,
    },
    #[error("manifest `desktop` block: {field}: {detail}")]
    DesktopInvalid { field: &'static str, detail: String },
}

fn validate_arg_defaults(args: &[Arg]) -> Result<(), (String, String)> {
    let optional_gap = args.iter().any(|arg| {
        arg.effective_binding() == ArgBinding::Positional
            && !arg.required
            && arg.default.is_none()
            && arg.default_from.is_none()
    });
    if optional_gap {
        if let Some(defaulted) = args.iter().find(|arg| {
            arg.effective_binding() == ArgBinding::Positional
                && (arg.default.is_some() || arg.default_from.is_some())
        }) {
            return Err((
                defaulted.name.clone(),
                "defaulted and omitted optional positional arguments cannot be mixed"
                    .to_string(),
            ));
        }
    }
    let has_optional_positional = args.iter().any(|arg| {
        arg.effective_binding() == ArgBinding::Positional && !arg.required
    });
    if has_optional_positional {
        if let Some(alias) = args.iter().find(|arg| arg.positional_alias) {
            return Err((
                alias.name.clone(),
                "positional_alias cannot be combined with optional positional arguments"
                    .to_string(),
            ));
        }
    }
    for (index, arg) in args.iter().enumerate() {
        if arg.positional_alias
            && (arg.effective_binding() != ArgBinding::Flag
                || arg.kind == ArgKind::Bool
                || arg.required
                || arg.repeatable)
        {
            return Err((
                arg.name.clone(),
                "positional_alias requires an optional, non-boolean, non-repeatable flag arg"
                    .to_string(),
            ));
        }
        if arg.positional_alias
            && args.iter().any(|candidate| {
            candidate.effective_binding() == ArgBinding::Positional
                && candidate.repeatable
            })
        {
            return Err((
            arg.name.clone(),
            "positional_alias cannot be combined with repeatable positionals".to_string(),
            ));
        }
        if arg.repeatable && arg.kind == ArgKind::Bool {
            return Err((
                arg.name.clone(),
                "repeatable boolean arguments are ambiguous".to_string(),
            ));
        }
        if arg.repeatable
            && (arg.default_from.is_some() || arg.trusted_resolver.is_some())
        {
            return Err((
                arg.name.clone(),
                "repeatable arguments cannot use default_from or trusted_resolver".to_string(),
            ));
        }
        if arg.repeatable
            && arg.effective_binding() == ArgBinding::Positional
            && args[index + 1..]
                .iter()
                .any(|later| later.effective_binding() == ArgBinding::Positional)
        {
            return Err((
                arg.name.clone(),
                "a repeatable positional argument must be the final positional argument"
                    .to_string(),
            ));
        }
        if !arg.choices.is_empty() {
            for choice in &arg.choices {
                if !arg.kind.accepts_default(choice) {
                    return Err((
                        arg.name.clone(),
                        format!("choice does not match arg kind `{:?}`", arg.kind),
                    ));
                }
            }
            if arg
                .choices
                .iter()
                .enumerate()
                .any(|(index, choice)| arg.choices[index + 1..].contains(choice))
            {
                return Err((arg.name.clone(), "choices must be unique".to_string()));
            }
        }
        if arg.effective_binding() == ArgBinding::Positional
            && !arg.required
            && args[index + 1..].iter().any(|later| {
                later.effective_binding() == ArgBinding::Positional && later.required
            })
        {
            return Err((
                arg.name.clone(),
                "optional positional arguments cannot precede required positional arguments"
                    .to_string(),
            ));
        }
        if arg.default.is_some() && arg.default_from.is_some() {
            return Err((
                arg.name.clone(),
                "declare only one of `default` or `default_from`".to_string(),
            ));
        }
        if arg.required && (arg.default.is_some() || arg.default_from.is_some()) {
            return Err((
                arg.name.clone(),
                "required arguments cannot declare defaults".to_string(),
            ));
        }
        if arg.effective_binding() == ArgBinding::Positional
            && (arg.default.is_some() || arg.default_from.is_some())
            && args[index + 1..]
                .iter()
                .any(|later| {
                    later.effective_binding() == ArgBinding::Positional && later.required
                })
        {
            return Err((
                arg.name.clone(),
                "defaulted arguments must follow all required arguments".to_string(),
            ));
        }
        if let Some(default) = &arg.default {
            if !arg.accepts_value(default) {
                return Err((
                    arg.name.clone(),
                    format!(
                        "value does not match arg kind `{:?}`, repeatability, or choices",
                        arg.kind
                    ),
                ));
            }
        }
        let Some(binding) = &arg.default_from else {
            continue;
        };
        let Some((source_index, source)) = args
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.name == binding.arg)
        else {
            return Err((
                arg.name.clone(),
                format!("references undeclared arg `{}`", binding.arg),
            ));
        };
        if source_index >= index {
            return Err((
                arg.name.clone(),
                format!(
                    "source `{}` must be declared before the defaulted arg",
                    binding.arg
                ),
            ));
        }
        if !matches!(
            source.kind,
            ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text
        ) || source.repeatable
        {
            return Err((
                arg.name.clone(),
                format!(
                    "source `{}` must be a non-repeatable string arg",
                    binding.arg
                ),
            ));
        }
        if !matches!(
            arg.kind,
            ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text
        ) {
            return Err((
                arg.name.clone(),
                "`default_from` can only populate a string arg".to_string(),
            ));
        }
        if !binding.prefix.is_empty() && arg.kind != ArgKind::Path {
            return Err((
                arg.name.clone(),
                "`prefix` is only supported for path defaults".to_string(),
            ));
        }
        if binding.prefix.chars().any(char::is_control) {
            return Err((
                arg.name.clone(),
                "`prefix` must not contain control characters".to_string(),
            ));
        }
        match binding.transform {
            ArgDefaultTransform::Identity => {
                if binding.fallback.is_some() {
                    return Err((
                        arg.name.clone(),
                        "`fallback` requires a transform that can produce no value".to_string(),
                    ));
                }
            }
            ArgDefaultTransform::UrlPathBasename => {
                if source.kind != ArgKind::Text || arg.kind != ArgKind::Path {
                    return Err((
                        arg.name.clone(),
                        "`url-path-basename` requires a text source and path destination"
                            .to_string(),
                    ));
                }
                if !binding
                    .fallback
                    .as_deref()
                    .is_some_and(is_safe_default_leaf)
                {
                    return Err((
                        arg.name.clone(),
                        "`url-path-basename` requires a safe single-component fallback".to_string(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_arg_aliases(args: &[Arg]) -> Result<(), (String, String)> {
    let mut options = BTreeMap::<String, String>::new();
    for arg in args {
        if arg.effective_binding() == ArgBinding::Flag {
            let canonical = format!("--{}", arg.name.replace('_', "-"));
            if let Some(existing) = options.insert(canonical.clone(), arg.name.clone()) {
                return Err((
                    arg.name.clone(),
                    format!("option `{canonical}` conflicts with arg `{existing}`"),
                ));
            }
        }
        for alias in &arg.aliases {
            let valid_short = alias.len() == 2
                && alias.starts_with('-')
                && alias.as_bytes()[1].is_ascii_alphanumeric();
            let valid_long = alias.strip_prefix("--").is_some_and(|name| {
                !name.is_empty()
                    && name.as_bytes()[0].is_ascii_lowercase()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            });
            if !valid_short && !valid_long {
                return Err((arg.name.clone(), format!("invalid option alias `{alias}`")));
            }
            if let Some(existing) = options.insert(alias.clone(), arg.name.clone()) {
                return Err((
                    arg.name.clone(),
                    format!("option alias `{alias}` conflicts with arg `{existing}`"),
                ));
            }
        }
    }
    Ok(())
}

fn is_safe_default_leaf(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

impl Manifest {
    /// Parse a manifest from JSON text.
    pub fn from_json(s: &str) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    /// The authoritative MCP service during the migration. New manifests use
    /// `mcp`; legacy packages continue through `session` until the final
    /// removal commit.
    pub fn mcp_service(&self) -> Option<&McpService> {
        self.mcp.as_ref().or(self.session.as_ref())
    }

    /// Verify every entry in `ai.tools[]` exists in the kernel
    /// catalog. The caller passes the catalog (typically
    /// `crate::ai::tools::list_names()`) so this module stays free
    /// of the `ai` dependency. No-op if the manifest has no `ai`
    /// block or an empty allowlist.
    pub fn validate_tools_against_catalog(&self, catalog: &[&str]) -> Result<(), ManifestError> {
        let Some(policy) = self.ai.as_ref() else {
            return Ok(());
        };
        for name in &policy.tools {
            if !catalog.contains(&name.as_str()) {
                return Err(ManifestError::AiUnknownTool { name: name.clone() });
            }
        }
        Ok(())
    }

    /// Validate the manifest's invariants. Called automatically by
    /// [`from_json`].
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !is_valid_id(&self.id) {
            return Err(ManifestError::InvalidId(self.id.clone()));
        }
        if self.mcp.is_some() && self.schema_version != Some(2) {
            return Err(ManifestError::McpSchemaVersion);
        }
        if self.mcp.is_some() && self.session.is_some() {
            return Err(ManifestError::McpLegacySessionConflict);
        }
        if let Some(service) = &self.mcp {
            let mut seen_apps = std::collections::BTreeSet::new();
            for app in &service.access.apps {
                if !is_valid_id(app) {
                    return Err(ManifestError::McpAccessInvalidApp { app: app.clone() });
                }
                if !seen_apps.insert(app.as_str()) {
                    return Err(ManifestError::McpAccessDuplicateApp { app: app.clone() });
                }
            }
        }
        self.name
            .validate()
            .map_err(|d| ManifestError::TopLevelTextInvalid {
                field: "name",
                detail: d,
            })?;

        if let Some(policy) = &self.ai {
            if policy.origins.is_empty() {
                return Err(ManifestError::AiPolicyNoOrigins);
            }
            // Shape-only dup check. Catalog membership is verified
            // by `validate_tools_against_catalog` (callers wire that
            // in to avoid a cycle between `caps` and `ai`).
            let mut seen = std::collections::BTreeSet::new();
            for name in &policy.tools {
                if !seen.insert(name.as_str()) {
                    return Err(ManifestError::AiDuplicateTool { name: name.clone() });
                }
            }
        }

        for (op_name, op) in &self.operations {
            // Operation keys appear in URLs, command-line invocations,
            // catalog lookups, and audit logs. Allow the standard
            // identifier alphabet plus `.` so namespacing like
            // `notes.create` works, but refuse anything else (`..`,
            // `/`, whitespace, NUL, etc.) — we don't want a hostile
            // manifest splicing path separators into a key that later
            // joins onto a filesystem path or HTTP route.
            if !is_valid_operation_key(op_name) {
                return Err(ManifestError::InvalidOperationKey(op_name.clone()));
            }
            op.label
                .validate()
                .map_err(|d| ManifestError::LocalizedTextInvalid {
                    op: op_name.clone(),
                    field: "label",
                    detail: d,
                })?;
            // Args must have unique names.
            let mut seen_args: BTreeMap<&str, &Arg> = BTreeMap::new();
            for arg in &op.args {
                if seen_args.insert(arg.name.as_str(), arg).is_some() {
                    return Err(ManifestError::DuplicateArg {
                        op: op_name.clone(),
                        arg: arg.name.clone(),
                    });
                }
                let trusted_shape_matches = match arg.trusted_resolver {
                    Some(TrustedArgResolver::EmailProvider) => {
                        self.id == "email" && arg.name == "provider" && arg.kind == ArgKind::Name
                    }
                    Some(TrustedArgResolver::EmailHost) => {
                        self.id == "email" && arg.name == "host" && arg.kind == ArgKind::Host
                    }
                    Some(TrustedArgResolver::CalendarProvider) => {
                        self.id == "calendar"
                            && arg.name == "provider"
                            && arg.kind == ArgKind::Name
                    }
                    Some(TrustedArgResolver::NtfyServer) => {
                        self.id == "gateway-ntfy"
                            && arg.name == "server"
                            && arg.kind == ArgKind::Text
                    }
                    None => true,
                };
                if arg.trusted_resolver.is_some()
                    && (!trusted_shape_matches
                        || arg.effective_binding() != ArgBinding::Flag
                        || arg.required
                        || arg.default.is_some()
                        || arg.default_from.is_some())
                {
                    return Err(ManifestError::ArgDefaultInvalid {
                        op: op_name.clone(),
                        arg: arg.name.clone(),
                        detail: "trusted resolver is restricted to its bundled app's optional provider flag"
                            .to_string(),
                    });
                }
            }
            if let Err((arg, detail)) = validate_arg_defaults(&op.args) {
                return Err(ManifestError::ArgDefaultInvalid {
                    op: op_name.clone(),
                    arg,
                    detail,
                });
            }
            if let Err((arg, detail)) = validate_arg_aliases(&op.args) {
                return Err(ManifestError::ArgDefaultInvalid {
                    op: op_name.clone(),
                    arg,
                    detail,
                });
            }
            for (index, arg) in op.args.iter().enumerate() {
                if let Some(condition) = &arg.required_when {
                    validate_required_when(arg, condition, index, &op.args, &seen_args).map_err(
                        |detail| ManifestError::ArgDefaultInvalid {
                            op: op_name.clone(),
                            arg: arg.name.clone(),
                            detail,
                        },
                    )?;
                }
            }
            // Needs must reference declared args and use compatible kinds.
            for (idx, need) in op.needs.iter().enumerate() {
                validate_need_condition(need, &seen_args).map_err(|detail| {
                    ManifestError::NeedInvalid {
                        op: op_name.clone(),
                        idx,
                        detail,
                    }
                })?;
                validate_literal_path_scopes(&need.scope).map_err(|detail| {
                    ManifestError::NeedInvalid {
                        op: op_name.clone(),
                        idx,
                        detail,
                    }
                })?;
                need.why
                    .validate()
                    .map_err(|d| ManifestError::NeedInvalid {
                        op: op_name.clone(),
                        idx,
                        detail: format!("why: {d}"),
                    })?;

                // AI verbs require an `ai` block on the manifest, and
                // `ai.bypass` is owner-only — apps can never declare it.
                let verb_str = need.verb.as_str();
                if verb_str == Verb::AI_BYPASS.as_str() {
                    return Err(ManifestError::AiBypassNotAllowedForApps {
                        field: "operations[].needs[].verb",
                    });
                }
                if verb_str.starts_with("ai.") && self.ai.is_none() {
                    return Err(ManifestError::AiNeedMissingPolicy {
                        op: op_name.clone(),
                        idx,
                        verb: verb_str.to_string(),
                    });
                }

                match &need.scope {
                    ScopeBinding::FromArg { arg, transform } => {
                        let a = seen_args.get(arg.as_str()).ok_or_else(|| {
                            ManifestError::NeedRefsUndeclaredArg {
                                op: op_name.clone(),
                                idx,
                                arg: arg.clone(),
                            }
                        })?;
                        let compatible = match transform {
                            ScopeTransform::Identity => a.kind.binds_to_scope(),
                            ScopeTransform::Parent => a.kind == ArgKind::Path,
                            ScopeTransform::UrlHost => a.kind == ArgKind::Text,
                        };
                        if !compatible {
                            return Err(ManifestError::NeedArgKindMismatch {
                                op: op_name.clone(),
                                idx,
                                verb: need.verb.as_str().to_string(),
                                arg: arg.clone(),
                                kind: a.kind,
                            });
                        }
                    }
                    ScopeBinding::FromArgMap { arg, values } => {
                        if !seen_args.contains_key(arg.as_str()) {
                            return Err(ManifestError::NeedRefsUndeclaredArg {
                                op: op_name.clone(),
                                idx,
                                arg: arg.clone(),
                            });
                        }
                        if values.is_empty() {
                            return Err(ManifestError::NeedInvalid {
                                op: op_name.clone(),
                                idx,
                                detail: "from-arg-map values must not be empty".to_string(),
                            });
                        }
                    }
                    ScopeBinding::FromArgOrWild { arg, wild_when } => {
                        let bound = seen_args.get(arg.as_str()).ok_or_else(|| {
                            ManifestError::NeedRefsUndeclaredArg {
                                op: op_name.clone(),
                                idx,
                                arg: arg.clone(),
                            }
                        })?;
                        if !bound.kind.binds_to_scope() {
                            return Err(ManifestError::NeedArgKindMismatch {
                                op: op_name.clone(),
                                idx,
                                verb: need.verb.as_str().to_string(),
                                arg: arg.clone(),
                                kind: bound.kind,
                            });
                        }
                        if !seen_args
                            .get(wild_when.as_str())
                            .is_some_and(|arg| arg.kind == ArgKind::Bool)
                        {
                            return Err(ManifestError::NeedInvalid {
                                op: op_name.clone(),
                                idx,
                                detail: format!(
                                    "wild_when `{wild_when}` must reference a bool arg"
                                ),
                            });
                        }
                    }
                    ScopeBinding::Fixed { scope: _ } => {}
                    ScopeBinding::Wild => {}
                }
                validate_optional_need_binding(need, &seen_args).map_err(|detail| {
                    ManifestError::NeedInvalid {
                        op: op_name.clone(),
                        idx,
                        detail,
                    }
                })?;
            }
        }

        if let Some(session) = self.mcp_service() {
            let mut seen_tools: std::collections::BTreeSet<&str> =
                std::collections::BTreeSet::new();
            for tool in &session.tools {
                if !is_valid_session_tool_name(&tool.name) {
                    return Err(ManifestError::SessionToolInvalidName {
                        tool: tool.name.clone(),
                    });
                }
                if !seen_tools.insert(tool.name.as_str()) {
                    return Err(ManifestError::SessionDuplicateTool {
                        name: tool.name.clone(),
                    });
                }
                tool.summary.validate().map_err(|d| {
                    ManifestError::SessionLocalizedTextInvalid {
                        tool: tool.name.clone(),
                        field: "summary",
                        detail: d,
                    }
                })?;

                let mut seen_args: BTreeMap<&str, &Arg> = BTreeMap::new();
                for arg in &tool.args {
                    if seen_args.insert(arg.name.as_str(), arg).is_some() {
                        return Err(ManifestError::SessionDuplicateArg {
                            tool: tool.name.clone(),
                            arg: arg.name.clone(),
                        });
                    }
                }
                if let Err((arg, detail)) = validate_arg_defaults(&tool.args) {
                    return Err(ManifestError::SessionArgDefaultInvalid {
                        tool: tool.name.clone(),
                        arg,
                        detail,
                    });
                }
                if let Err((arg, detail)) = validate_arg_aliases(&tool.args) {
                    return Err(ManifestError::SessionArgDefaultInvalid {
                        tool: tool.name.clone(),
                        arg,
                        detail,
                    });
                }
                for (index, arg) in tool.args.iter().enumerate() {
                    if let Some(condition) = &arg.required_when {
                        validate_required_when(arg, condition, index, &tool.args, &seen_args)
                            .map_err(|detail| ManifestError::SessionArgDefaultInvalid {
                                tool: tool.name.clone(),
                                arg: arg.name.clone(),
                                detail,
                            })?;
                    }
                }
                if let Some(arg) = tool
                    .args
                    .iter()
                    .find(|arg| {
                        arg.default_from.is_some()
                            || arg.trusted_resolver.is_some()
                            || !arg.aliases.is_empty()
                            || arg.positional_alias
                    })
                {
                    return Err(ManifestError::SessionArgDefaultInvalid {
                        tool: tool.name.clone(),
                        arg: arg.name.clone(),
                        detail: "CLI aliases, default_from, and trusted resolvers are only supported for one-shot operations"
                            .to_string(),
                    });
                }
                for (idx, need) in tool.needs.iter().enumerate() {
                    validate_need_condition(need, &seen_args).map_err(|detail| {
                        ManifestError::SessionNeedInvalid {
                            tool: tool.name.clone(),
                            idx,
                            detail,
                        }
                    })?;
                    validate_literal_path_scopes(&need.scope).map_err(|detail| {
                        ManifestError::SessionNeedInvalid {
                            tool: tool.name.clone(),
                            idx,
                            detail,
                        }
                    })?;
                    need.why
                        .validate()
                        .map_err(|d| ManifestError::SessionNeedInvalid {
                            tool: tool.name.clone(),
                            idx,
                            detail: format!("why: {d}"),
                        })?;

                    let verb_str = need.verb.as_str();
                    if verb_str == Verb::AI_BYPASS.as_str() {
                        return Err(ManifestError::AiBypassNotAllowedForApps {
                            field: "session.tools[].needs[].verb",
                        });
                    }
                    if verb_str.starts_with("ai.") && self.ai.is_none() {
                        return Err(ManifestError::SessionAiNeedMissingPolicy {
                            tool: tool.name.clone(),
                            idx,
                            verb: verb_str.to_string(),
                        });
                    }

                    match &need.scope {
                        ScopeBinding::FromArg { arg, transform } => {
                            let a = seen_args.get(arg.as_str()).ok_or_else(|| {
                                ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool.name.clone(),
                                    idx,
                                    arg: arg.clone(),
                                }
                            })?;
                            let compatible = match transform {
                                ScopeTransform::Identity => a.kind.binds_to_scope(),
                                ScopeTransform::Parent => a.kind == ArgKind::Path,
                                ScopeTransform::UrlHost => a.kind == ArgKind::Text,
                            };
                            if !compatible {
                                return Err(ManifestError::SessionNeedArgKindMismatch {
                                    tool: tool.name.clone(),
                                    idx,
                                    verb: need.verb.as_str().to_string(),
                                    arg: arg.clone(),
                                    kind: a.kind,
                                });
                            }
                        }
                        ScopeBinding::FromArgMap { arg, values } => {
                            if !seen_args.contains_key(arg.as_str()) {
                                return Err(ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool.name.clone(),
                                    idx,
                                    arg: arg.clone(),
                                });
                            }
                            if values.is_empty() {
                                return Err(ManifestError::SessionNeedInvalid {
                                    tool: tool.name.clone(),
                                    idx,
                                    detail: "from-arg-map values must not be empty".to_string(),
                                });
                            }
                        }
                        ScopeBinding::FromArgOrWild { arg, wild_when } => {
                            let bound = seen_args.get(arg.as_str()).ok_or_else(|| {
                                ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool.name.clone(),
                                    idx,
                                    arg: arg.clone(),
                                }
                            })?;
                            if !bound.kind.binds_to_scope() {
                                return Err(ManifestError::SessionNeedArgKindMismatch {
                                    tool: tool.name.clone(),
                                    idx,
                                    verb: need.verb.as_str().to_string(),
                                    arg: arg.clone(),
                                    kind: bound.kind,
                                });
                            }
                            if !seen_args
                                .get(wild_when.as_str())
                                .is_some_and(|arg| arg.kind == ArgKind::Bool)
                            {
                                return Err(ManifestError::SessionNeedInvalid {
                                    tool: tool.name.clone(),
                                    idx,
                                    detail: format!(
                                        "wild_when `{wild_when}` must reference a bool arg"
                                    ),
                                });
                            }
                        }
                        ScopeBinding::Fixed { scope: _ } => {}
                        ScopeBinding::Wild => {}
                    }
                    validate_optional_need_binding(need, &seen_args).map_err(|detail| {
                        ManifestError::SessionNeedInvalid {
                            tool: tool.name.clone(),
                            idx,
                            detail,
                        }
                    })?;
                }
            }
        }

        if let Some(desktop) = &self.desktop {
            if let Some(name) = &desktop.name {
                name.validate().map_err(|d| ManifestError::DesktopInvalid {
                    field: "name",
                    detail: d,
                })?;
            }
            // `exec` and `icon` land verbatim on `.desktop` lines, so
            // reject control characters (newlines especially — they
            // could inject extra desktop-entry keys).
            if desktop.exec.trim().is_empty() {
                return Err(ManifestError::DesktopInvalid {
                    field: "exec",
                    detail: "must not be empty".into(),
                });
            }
            if desktop.exec.chars().any(|c| c.is_control()) {
                return Err(ManifestError::DesktopInvalid {
                    field: "exec",
                    detail: "must not contain control characters".into(),
                });
            }
            if let Some(icon) = &desktop.icon {
                if icon.chars().any(|c| c.is_control()) {
                    return Err(ManifestError::DesktopInvalid {
                        field: "icon",
                        detail: "must not contain control characters".into(),
                    });
                }
            }
            // `;` is the freedesktop list separator; a value containing
            // it (or a control char) would corrupt the generated file.
            for cat in &desktop.categories {
                if cat.is_empty() || cat.contains(';') || cat.chars().any(|c| c.is_control()) {
                    return Err(ManifestError::DesktopInvalid {
                        field: "categories",
                        detail: format!("invalid category `{cat}`"),
                    });
                }
            }
            for mt in &desktop.mime_types {
                if mt.is_empty() || mt.contains(';') || mt.chars().any(|c| c.is_control()) {
                    return Err(ManifestError::DesktopInvalid {
                        field: "mime_types",
                        detail: format!("invalid mime type `{mt}`"),
                    });
                }
            }
        }
        Ok(())
    }
    /// Resolve effective argument values and aligned capabilities together.
    pub fn resolve_operation_call(
        &self,
        op_name: &str,
        supplied: &BTreeMap<String, serde_json::Value>,
        paths: &super::args::PathContext,
    ) -> Result<EffectiveCall, ManifestError> {
        let operation =
            self.operations
                .get(op_name)
                .ok_or_else(|| ManifestError::NeedInvalid {
                    op: op_name.to_string(),
                    idx: 0,
                    detail: "unknown operation".to_string(),
                })?;
        let (mut values, defaulted) =
            resolve_effective_args(&operation.args, supplied, Some(paths)).map_err(|detail| {
                ManifestError::NeedInvalid {
                    op: op_name.to_string(),
                    idx: 0,
                    detail,
                }
            })?;
        canonicalize_url_scope_args(&operation.args, &operation.needs, &mut values).map_err(
            |detail| ManifestError::NeedInvalid {
                op: op_name.to_string(),
                idx: 0,
                detail,
            },
        )?;
        let needs = self.resolve_needs(op_name, &values)?;
        Ok(EffectiveCall {
            values,
            needs,
            defaulted,
        })
    }

    /// `op_name` selects which operation; `args` is a map of arg name
    /// → JSON-encoded value (the same shape the bridge already passes
    /// through). Unknown args produce `None`; needs that bind to a
    /// missing arg are reported in the error.
    pub fn resolve_needs(
        &self,
        op_name: &str,
        args: &BTreeMap<String, serde_json::Value>,
    ) -> Result<Vec<Vec<super::cap::Cap>>, ManifestError> {
        let op = self
            .operations
            .get(op_name)
            .ok_or_else(|| ManifestError::NeedInvalid {
                op: op_name.to_string(),
                idx: 0,
                detail: "unknown operation".into(),
            })?;
        let (mut args, _) = resolve_effective_args(&op.args, args, None).map_err(|detail| {
            ManifestError::NeedInvalid {
                op: op_name.to_string(),
                idx: 0,
                detail,
            }
        })?;
        canonicalize_url_scope_args(&op.args, &op.needs, &mut args).map_err(|detail| {
            ManifestError::NeedInvalid {
                op: op_name.to_string(),
                idx: 0,
                detail,
            }
        })?;
        let mut out = Vec::with_capacity(op.needs.len());
        for (idx, need) in op.needs.iter().enumerate() {
            if !condition_applies(need.when.as_ref(), &args) {
                out.push(Vec::new());
                continue;
            }
            let scopes = match &need.scope {
                ScopeBinding::FromArg { arg, transform } => {
                    let val = args.get(arg).ok_or_else(|| ManifestError::NeedInvalid {
                        op: op_name.to_string(),
                        idx,
                        detail: format!("arg `{arg}` not supplied at call time"),
                    })?;
                    let arg_decl = op.args.iter().find(|a| a.name == *arg).ok_or_else(|| {
                        ManifestError::NeedRefsUndeclaredArg {
                            op: op_name.to_string(),
                            idx,
                            arg: arg.clone(),
                        }
                    })?;
                    scopes_from_arg_value(arg_decl, val, *transform).ok_or_else(|| {
                        ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!(
                                "arg `{arg}` value is not a {kind:?}",
                                kind = arg_decl.kind
                            ),
                        }
                    })?
                }
                ScopeBinding::FromArgMap { arg, values } => {
                    let value = args.get(arg).ok_or_else(|| ManifestError::NeedInvalid {
                        op: op_name.to_string(),
                        idx,
                        detail: format!("arg `{arg}` was not supplied"),
                    })?;
                    mapped_scopes(value, values).map_err(|detail| {
                        ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` {detail}"),
                        }
                    })?
                }
                ScopeBinding::FromArgOrWild { arg, wild_when } => {
                    if args
                        .get(wild_when)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        vec![Scope::Wild]
                    } else {
                        let value = args.get(arg).ok_or_else(|| ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` not supplied at call time"),
                        })?;
                        let decl =
                            op.args
                                .iter()
                                .find(|decl| decl.name == *arg)
                                .ok_or_else(|| ManifestError::NeedRefsUndeclaredArg {
                                    op: op_name.to_string(),
                                    idx,
                                    arg: arg.clone(),
                                })?;
                        scopes_from_arg_value(decl, value, ScopeTransform::Identity).ok_or_else(|| {
                            ManifestError::NeedInvalid {
                                op: op_name.to_string(),
                                idx,
                                detail: format!("arg `{arg}` cannot populate a scope"),
                            }
                        })?
                    }
                }
                ScopeBinding::Fixed { scope } => vec![scope.clone()],
                ScopeBinding::Wild => vec![Scope::Wild],
            };
            out.push(
                scopes
                    .into_iter()
                    .map(|scope| super::cap::Cap::new(need.verb, scope))
                    .collect(),
            );
        }
        Ok(out)
    }
    /// Resolve a session tool's needs into concrete [`Cap`](super::cap::Cap)s
    /// for a specific MCP `tools/call` invocation. Mirrors
    /// [`resolve_needs`](Self::resolve_needs) but reads from the
    /// `session.tools[]` table instead of `operations`. Returns
    /// `NeedInvalid` if the manifest has no session block or the tool
    /// name is unknown.
    pub fn resolve_session_tool_needs(
        &self,
        tool_name: &str,
        args: &BTreeMap<String, serde_json::Value>,
    ) -> Result<Vec<Vec<super::cap::Cap>>, ManifestError> {
        let args = self.resolve_session_tool_args(tool_name, args)?;
        let session = self
            .mcp_service()
            .ok_or_else(|| ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail: "manifest has no `session` block".into(),
            })?;
        let tool = session
            .tools
            .iter()
            .find(|t| t.name == tool_name)
            .ok_or_else(|| ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail: "unknown session tool".into(),
            })?;
        let mut out = Vec::with_capacity(tool.needs.len());
        for (idx, need) in tool.needs.iter().enumerate() {
            if !condition_applies(need.when.as_ref(), &args) {
                out.push(Vec::new());
                continue;
            }
            let scopes =
                match &need.scope {
                    ScopeBinding::FromArg { arg, transform } => {
                        let val =
                            args.get(arg)
                                .ok_or_else(|| ManifestError::SessionNeedInvalid {
                                    tool: tool_name.to_string(),
                                    idx,
                                    detail: format!("arg `{arg}` not supplied at call time"),
                                })?;
                        let arg_decl =
                            tool.args.iter().find(|a| a.name == *arg).ok_or_else(|| {
                                ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool_name.to_string(),
                                    idx,
                                    arg: arg.clone(),
                                }
                            })?;
                        scopes_from_arg_value(arg_decl, val, *transform).ok_or_else(|| {
                            ManifestError::SessionNeedInvalid {
                                tool: tool_name.to_string(),
                                idx,
                                detail: format!(
                                    "arg `{arg}` value is not a {kind:?}",
                                    kind = arg_decl.kind
                                ),
                            }
                        })?
                    }
                    ScopeBinding::FromArgMap { arg, values } => {
                        let value =
                            args.get(arg)
                                .ok_or_else(|| ManifestError::SessionNeedInvalid {
                                    tool: tool_name.to_string(),
                                    idx,
                                    detail: format!("arg `{arg}` was not supplied"),
                                })?;
                        mapped_scopes(value, values).map_err(|detail| {
                            ManifestError::SessionNeedInvalid {
                                tool: tool_name.to_string(),
                                idx,
                                detail: format!("arg `{arg}` {detail}"),
                            }
                        })?
                    }
                    ScopeBinding::FromArgOrWild { arg, wild_when } => {
                        if args
                            .get(wild_when)
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                        {
                            vec![Scope::Wild]
                        } else {
                            let value =
                                args.get(arg)
                                    .ok_or_else(|| ManifestError::SessionNeedInvalid {
                                        tool: tool_name.to_string(),
                                        idx,
                                        detail: format!("arg `{arg}` not supplied at call time"),
                                    })?;
                            let decl = tool.args.iter().find(|decl| decl.name == *arg).ok_or_else(
                                || ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool_name.to_string(),
                                    idx,
                                    arg: arg.clone(),
                                },
                            )?;
                            scopes_from_arg_value(decl, value, ScopeTransform::Identity).ok_or_else(|| {
                                ManifestError::SessionNeedInvalid {
                                    tool: tool_name.to_string(),
                                    idx,
                                    detail: format!("arg `{arg}` cannot populate a scope"),
                                }
                            })?
                        }
                    }
                    ScopeBinding::Fixed { scope } => vec![scope.clone()],
                    ScopeBinding::Wild => vec![Scope::Wild],
                };
            out.push(
                scopes
                    .into_iter()
                    .map(|scope| super::cap::Cap::new(need.verb, scope))
                    .collect(),
            );
        }
        Ok(out)
    }

    pub fn resolve_session_tool_call(
        &self,
        tool_name: &str,
        supplied: &BTreeMap<String, serde_json::Value>,
        paths: &super::args::PathContext,
    ) -> Result<EffectiveCall, ManifestError> {
        let tool = self
            .mcp_service()
            .and_then(|session| session.tools.iter().find(|tool| tool.name == tool_name))
            .ok_or_else(|| ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail: "unknown session tool".to_string(),
            })?;
        let (mut values, defaulted) =
            resolve_effective_args(&tool.args, supplied, Some(paths)).map_err(|detail| {
                ManifestError::SessionNeedInvalid {
                    tool: tool_name.to_string(),
                    idx: 0,
                    detail,
                }
            })?;
        canonicalize_url_scope_args(&tool.args, &tool.needs, &mut values).map_err(|detail| {
            ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail,
            }
        })?;
        let needs = self.resolve_session_tool_needs(tool_name, &values)?;
        Ok(EffectiveCall {
            values,
            needs,
            defaulted,
        })
    }

    /// Validate a session call and apply every literal manifest default.
    /// This map is shared by capability derivation, transient authority,
    /// and the forwarded MCP invocation.
    pub fn resolve_session_tool_args(
        &self,
        tool_name: &str,
        args: &BTreeMap<String, serde_json::Value>,
    ) -> Result<BTreeMap<String, serde_json::Value>, ManifestError> {
        let session = self
            .mcp_service()
            .ok_or_else(|| ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail: "manifest has no `session` block".to_string(),
            })?;
        let tool = session
            .tools
            .iter()
            .find(|tool| tool.name == tool_name)
            .ok_or_else(|| ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail: "unknown session tool".to_string(),
            })?;
        let (mut resolved, _) = resolve_effective_args(&tool.args, args, None).map_err(|detail| {
            ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail,
            }
        })?;
        canonicalize_url_scope_args(&tool.args, &tool.needs, &mut resolved).map_err(|detail| {
            ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail,
            }
        })?;
        Ok(resolved)
    }
}

fn validate_need_condition(
    need: &Need,
    args: &BTreeMap<&str, &Arg>,
) -> Result<(), String> {
    let Some(condition) = &need.when else {
        return Ok(());
    };
    let (arg_name, expected) = match condition {
        NeedCondition::ArgPresent { arg } => (arg, None),
        NeedCondition::ArgEquals { arg, value }
        | NeedCondition::ArgNotEquals { arg, value } => (arg, Some(value)),
    };
    let declaration = args
        .get(arg_name.as_str())
        .ok_or_else(|| format!("condition references undeclared arg `{arg_name}`"))?;
    if let Some(value) = expected {
        if declaration.repeatable {
            return Err(format!(
                "arg-equals cannot target repeatable arg `{arg_name}`"
            ));
        }
        if value.is_null() || !declaration.accepts_scalar(value) {
            return Err(format!(
                "condition value for `{arg_name}` does not match arg kind `{:?}`",
                declaration.kind
            ));
        }
    }
    Ok(())
}

fn validate_required_when(
    declaration: &Arg,
    condition: &NeedCondition,
    index: usize,
    declarations: &[Arg],
    args: &BTreeMap<&str, &Arg>,
) -> Result<(), String> {
    if declaration.required {
        return Err("required_when cannot be combined with required=true".to_string());
    }
    if declaration.default.is_some()
        || declaration.default_from.is_some()
        || declaration.trusted_resolver.is_some()
        || declaration.repeatable
    {
        return Err(
            "required_when arguments cannot be repeatable or declare defaults or trusted resolvers"
                .into(),
        );
    }
    let referenced = match condition {
        NeedCondition::ArgPresent { arg }
        | NeedCondition::ArgEquals { arg, .. }
        | NeedCondition::ArgNotEquals { arg, .. } => arg,
    };
    if referenced == &declaration.name {
        return Err("required_when cannot reference its own argument".to_string());
    }
    let referenced_index = declarations
        .iter()
        .position(|candidate| candidate.name == *referenced)
        .ok_or_else(|| format!("required_when references undeclared arg `{referenced}`"))?;
    if referenced_index >= index {
        return Err("required_when must reference an earlier argument".to_string());
    }
    let synthetic_need = Need {
        verb: Verb::SYS_OBSERVE,
        scope: ScopeBinding::Wild,
        when: Some(condition.clone()),
        why: LocalizedText::default(),
    };
    validate_need_condition(&synthetic_need, args)
}

fn validate_optional_need_binding(
    need: &Need,
    args: &BTreeMap<&str, &Arg>,
) -> Result<(), String> {
    let bound_arg = match &need.scope {
        ScopeBinding::FromArg { arg, .. }
        | ScopeBinding::FromArgMap { arg, .. }
        | ScopeBinding::FromArgOrWild { arg, .. } => arg,
        ScopeBinding::Fixed { .. } | ScopeBinding::Wild => return Ok(()),
    };
    let Some(declaration) = args.get(bound_arg.as_str()) else {
        return Ok(());
    };
    let guaranteed = declaration.required
        || declaration.default.is_some()
        || declaration.default_from.is_some()
        || declaration.trusted_resolver.is_some()
        || declaration.kind == ArgKind::Bool;
    let explicitly_guarded = matches!(
        &need.when,
        Some(
            NeedCondition::ArgPresent { arg }
                | NeedCondition::ArgEquals { arg, .. }
                | NeedCondition::ArgNotEquals { arg, .. }
        )
            if arg == bound_arg
    );
    if guaranteed || explicitly_guarded {
        Ok(())
    } else {
        Err(format!(
            "capability binding to optional arg `{bound_arg}` requires an explicit condition"
        ))
    }
}

pub(crate) fn condition_applies(
    condition: Option<&NeedCondition>,
    args: &BTreeMap<String, serde_json::Value>,
) -> bool {
    match condition {
        None => true,
        Some(NeedCondition::ArgPresent { arg }) => args
            .get(arg)
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty())),
        Some(NeedCondition::ArgEquals { arg, value }) => args.get(arg) == Some(value),
        Some(NeedCondition::ArgNotEquals { arg, value }) => {
            args.get(arg).is_some_and(|actual| actual != value)
        }
    }
}

fn validate_literal_path_scopes(binding: &ScopeBinding) -> Result<(), String> {
    let invalid = match binding {
        ScopeBinding::Fixed {
            scope: Scope::Path(path),
        } => path.contains('$').then_some(path),
        ScopeBinding::FromArgMap { values, .. } => values.values().find_map(|scope| match scope {
            Scope::Path(path) if path.contains('$') => Some(path),
            _ => None,
        }),
        _ => None,
    };
    match invalid {
        Some(path) => Err(format!(
            "path scope `{path}` contains an unsupported environment placeholder"
        )),
        None => Ok(()),
    }
}

fn is_valid_session_tool_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.'
    })
}

fn scope_from_arg_value(kind: ArgKind, value: &serde_json::Value) -> Option<Scope> {
    let s = value.as_str()?;
    Some(match kind {
        ArgKind::Path => Scope::path(s),
        ArgKind::Host => Scope::host(s),
        ArgKind::Name => Scope::name(s),
        _ => return None,
    })
}

fn scopes_from_arg_value(
    arg: &Arg,
    value: &serde_json::Value,
    transform: ScopeTransform,
) -> Option<Vec<Scope>> {
    let scope = |value: &serde_json::Value| {
        if transform == ScopeTransform::UrlHost {
            let raw = value.as_str()?;
            return canonical_url_and_scope(raw).map(|(_, scope)| scope);
        }
        let scope = scope_from_arg_value(arg.kind, value)?;
        match (transform, scope) {
            (ScopeTransform::Identity, scope) => Some(scope),
            (ScopeTransform::Parent, Scope::Path(path)) => {
                let path = std::path::Path::new(&path);
                let parent = path.parent().unwrap_or(path);
                Some(Scope::path(parent.to_string_lossy()))
            }
            (ScopeTransform::Parent, _) => None,
            (ScopeTransform::UrlHost, _) => unreachable!(),
        }
    };
    if arg.repeatable {
        value.as_array()?.iter().map(scope).collect()
    } else {
        scope(value).map(|scope| vec![scope])
    }
}

pub(crate) fn canonicalize_url_scope_args(
    declarations: &[Arg],
    needs: &[Need],
    values: &mut BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    for arg_name in needs.iter().filter_map(|need| match &need.scope {
        ScopeBinding::FromArg {
            arg,
            transform: ScopeTransform::UrlHost,
        } => Some(arg),
        _ => None,
    }) {
        let declaration = declarations
            .iter()
            .find(|declaration| declaration.name == *arg_name)
            .ok_or_else(|| format!("url-host references undeclared arg `{arg_name}`"))?;
        let Some(value) = values.get_mut(arg_name) else {
            continue;
        };
        if declaration.repeatable {
            let items = value
                .as_array_mut()
                .ok_or_else(|| format!("repeatable URL arg `{arg_name}` is not an array"))?;
            for item in items {
                let raw = item
                    .as_str()
                    .ok_or_else(|| format!("URL arg `{arg_name}` is not text"))?;
                *item = serde_json::Value::String(
                    canonical_url_and_scope(raw)
                        .ok_or_else(|| format!("URL arg `{arg_name}` is invalid"))?
                        .0,
                );
            }
        } else {
            let raw = value
                .as_str()
                .ok_or_else(|| format!("URL arg `{arg_name}` is not text"))?;
            *value = serde_json::Value::String(
                canonical_url_and_scope(raw)
                    .ok_or_else(|| format!("URL arg `{arg_name}` is invalid"))?
                    .0,
            );
        }
    }
    Ok(())
}

fn canonical_url_and_scope(raw: &str) -> Option<(String, Scope)> {
    let normalized;
    let raw = if raw.contains("://") {
        raw
    } else {
        normalized = format!("https://{raw}");
        &normalized
    };
    let explicit_port = explicit_url_port(raw);
    let parsed = url::Url::parse(raw).ok()?;
    let host = parsed.host()?;
    let rendered = match host {
        url::Host::Domain(host) => host.to_string(),
        url::Host::Ipv4(host) => host.to_string(),
        url::Host::Ipv6(host) => format!("[{host}]"),
    };
    let port = match explicit_port.or_else(|| parsed.port()) {
        Some(port) => port,
        None => match parsed.scheme() {
            "http" => 80,
            "https" => 443,
            _ => return None,
        },
    };
    let mut canonical = parsed.to_string();
    if !matches!(parsed.scheme(), "http" | "https")
        && explicit_port.is_some()
        && parsed.port().is_none()
    {
        let authority_start = canonical.find("://")? + 3;
        let authority_end = canonical[authority_start..]
            .find(['/', '?', '#'])
            .map(|offset| authority_start + offset)
            .unwrap_or(canonical.len());
        canonical.insert_str(authority_end, &format!(":{port}"));
    }
    Some((canonical, Scope::host(format!("{rendered}:{port}"))))
}

fn explicit_url_port(raw: &str) -> Option<u16> {
    let authority = raw
        .split_once("://")?
        .1
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?;
    let port = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split_once(']')?.1.strip_prefix(':')?
    } else {
        authority.rsplit_once(':')?.1
    };
    port.parse().ok()
}

fn mapped_scopes(
    value: &serde_json::Value,
    mappings: &BTreeMap<String, Scope>,
) -> Result<Vec<Scope>, String> {
    let values = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value));
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| "must be a string or string array".to_string())?;
            mappings
                .get(value)
                .cloned()
                .ok_or_else(|| format!("has unmapped value `{value}`"))
        })
        .collect()
}

fn is_valid_id(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Operation keys allow the same alphabet as ids plus `.` so
/// namespaced operations like `notes.create` are legal. They must
/// not be empty, must start with a lowercase letter, and must not
/// contain `..` (which would be a path-traversal vector if a key is
/// ever joined onto a filesystem path).
fn is_valid_operation_key(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    if s.contains("..") {
        return false;
    }
    bytes.all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.' || b == b'-'
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/manifest.rs"
    ));
}

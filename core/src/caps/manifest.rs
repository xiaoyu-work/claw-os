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
    /// Declared input parameters. Order is significant for the UI.
    #[serde(default)]
    pub args: Vec<Arg>,
    /// Capability requirements. Empty means the operation is purely
    /// local (no gated action); the kernel still records the call in
    /// the audit log but does not prompt for permission.
    #[serde(default)]
    pub needs: Vec<Need>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Arg {
    /// Identifier referenced by `from-arg` scope bindings.
    pub name: String,
    /// What kind of value this arg holds; the UI uses this to pick a
    /// widget and to validate the input.
    pub kind: ArgKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Human-readable help. Optional.
    #[serde(default)]
    pub label: LocalizedText,
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
    /// Boolean toggle.
    Bool,
}

impl ArgKind {
    /// Returns true if values of this kind can populate a [`Scope`].
    pub fn binds_to_scope(self) -> bool {
        matches!(self, ArgKind::Path | ArgKind::Host | ArgKind::Name)
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

    /// Tools the app advertises through this session. The list is
    /// authoritative: the kernel only forwards `tools/call` requests
    /// for names that appear here, and runs the declared `needs[]` as
    /// the cap gate before forwarding.
    #[serde(default)]
    pub tools: Vec<SessionTool>,
}

/// Wire protocol for [`Session`]. Stdio is the de-facto MCP default
/// and the one our integration already implements.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionTransport {
    #[default]
    Stdio,
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
    /// Reason shown to the user in the approval dialog. Authors are
    /// expected to write this in plain language ("Read the file you
    /// asked me to summarise"), not jargon.
    pub why: LocalizedText,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScopeBinding {
    FromArg { arg: String },
    FromArgMap {
        arg: String,
        values: BTreeMap<String, Scope>,
    },
    FromArgOrWild { arg: String, wild_when: String },
    Fixed { scope: Scope },
    Wild,
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
    TopLevelTextInvalid {
        field: &'static str,
        detail: String,
    },
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
    AiBypassNotAllowedForApps {
        field: &'static str,
    },
    #[error(
        "manifest `ai.tools[]`: unknown tool `{name}` — not in the kernel \
         catalog. Run `cos ai tools` to list known tools."
    )]
    AiUnknownTool { name: String },
    #[error("manifest `ai.tools[]`: tool `{name}` declared twice")]
    AiDuplicateTool { name: String },
    #[error("session tool `{tool}`: name must match [a-z][a-z0-9._-]* — got `{tool}`")]
    SessionToolInvalidName { tool: String },
    #[error("session tool `{name}` declared twice")]
    SessionDuplicateTool { name: String },
    #[error("session tool `{tool}`: arg `{arg}` declared twice")]
    SessionDuplicateArg { tool: String, arg: String },
    #[error("session tool `{tool}`: need #{idx} references undeclared arg `{arg}`")]
    SessionNeedRefsUndeclaredArg { tool: String, idx: usize, arg: String },
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
    DesktopInvalid {
        field: &'static str,
        detail: String,
    },
}

impl Manifest {
    /// Parse a manifest from JSON text.
    pub fn from_json(s: &str) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    /// Verify every entry in `ai.tools[]` exists in the kernel
    /// catalog. The caller passes the catalog (typically
    /// `crate::ai::tools::list_names()`) so this module stays free
    /// of the `ai` dependency. No-op if the manifest has no `ai`
    /// block or an empty allowlist.
    pub fn validate_tools_against_catalog(
        &self,
        catalog: &[&str],
    ) -> Result<(), ManifestError> {
        let Some(policy) = self.ai.as_ref() else {
            return Ok(());
        };
        for name in &policy.tools {
            if !catalog.iter().any(|c| *c == name.as_str()) {
                return Err(ManifestError::AiUnknownTool {
                    name: name.clone(),
                });
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
        self.name.validate().map_err(|d| ManifestError::TopLevelTextInvalid {
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
                    return Err(ManifestError::AiDuplicateTool {
                        name: name.clone(),
                    });
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
            }
            // Needs must reference declared args and use compatible kinds.
            for (idx, need) in op.needs.iter().enumerate() {
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
                    ScopeBinding::FromArg { arg } => {
                        let a = seen_args.get(arg.as_str()).ok_or_else(|| {
                            ManifestError::NeedRefsUndeclaredArg {
                                op: op_name.clone(),
                                idx,
                                arg: arg.clone(),
                            }
                        })?;
                        if !a.kind.binds_to_scope() {
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
            }
        }

        if let Some(session) = &self.session {
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
                for (idx, need) in tool.needs.iter().enumerate() {
                    need.why.validate().map_err(|d| {
                        ManifestError::SessionNeedInvalid {
                            tool: tool.name.clone(),
                            idx,
                            detail: format!("why: {d}"),
                        }
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
                        ScopeBinding::FromArg { arg } => {
                            let a = seen_args.get(arg.as_str()).ok_or_else(|| {
                                ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool.name.clone(),
                                    idx,
                                    arg: arg.clone(),
                                }
                            })?;
                            if !a.kind.binds_to_scope() {
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
                                return Err(
                                    ManifestError::SessionNeedRefsUndeclaredArg {
                                        tool: tool.name.clone(),
                                        idx,
                                        arg: arg.clone(),
                                    },
                                );
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
                if cat.is_empty() || cat.contains(';') || cat.chars().any(|c| c.is_control())
                {
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
    /// for a specific invocation.
    ///
    /// `op_name` selects which operation; `args` is a map of arg name
    /// → JSON-encoded value (the same shape the bridge already passes
    /// through). Unknown args produce `None`; needs that bind to a
    /// missing arg are reported in the error.
    pub fn resolve_needs(
        &self,
        op_name: &str,
        args: &BTreeMap<String, serde_json::Value>,
    ) -> Result<Vec<super::cap::Cap>, ManifestError> {
        let op = self.operations.get(op_name).ok_or_else(|| {
            ManifestError::NeedInvalid {
                op: op_name.to_string(),
                idx: 0,
                detail: "unknown operation".into(),
            }
        })?;
        let mut out = Vec::with_capacity(op.needs.len());
        for (idx, need) in op.needs.iter().enumerate() {
            let scope = match &need.scope {
                ScopeBinding::FromArg { arg } => {
                    let val = args.get(arg).ok_or_else(|| {
                        ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` not supplied at call time"),
                        }
                    })?;
                    let arg_decl = op
                        .args
                        .iter()
                        .find(|a| a.name == *arg)
                        .ok_or_else(|| ManifestError::NeedRefsUndeclaredArg {
                            op: op_name.to_string(),
                            idx,
                            arg: arg.clone(),
                        })?;
                    scope_from_arg_value(arg_decl.kind, val).ok_or_else(|| {
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
                    let value = args
                        .get(arg)
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` must be a string"),
                        })?;
                    values.get(value).cloned().ok_or_else(|| {
                        ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` has unmapped value `{value}`"),
                        }
                    })?
                }
                ScopeBinding::FromArgOrWild { arg, wild_when } => {
                    if args
                        .get(wild_when)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        Scope::Wild
                    } else {
                        let value = args.get(arg).ok_or_else(|| {
                            ManifestError::NeedInvalid {
                                op: op_name.to_string(),
                                idx,
                                detail: format!("arg `{arg}` not supplied at call time"),
                            }
                        })?;
                        let decl = op
                            .args
                            .iter()
                            .find(|decl| decl.name == *arg)
                            .ok_or_else(|| ManifestError::NeedRefsUndeclaredArg {
                                op: op_name.to_string(),
                                idx,
                                arg: arg.clone(),
                            })?;
                        scope_from_arg_value(decl.kind, value).ok_or_else(|| {
                            ManifestError::NeedInvalid {
                                op: op_name.to_string(),
                                idx,
                                detail: format!("arg `{arg}` cannot populate a scope"),
                            }
                        })?
                    }
                }
                ScopeBinding::Fixed { scope } => scope.clone(),
                ScopeBinding::Wild => Scope::Wild,
            };
            out.push(super::cap::Cap::new(need.verb, scope));
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
    ) -> Result<Vec<super::cap::Cap>, ManifestError> {
        let session = self.session.as_ref().ok_or_else(|| {
            ManifestError::SessionNeedInvalid {
                tool: tool_name.to_string(),
                idx: 0,
                detail: "manifest has no `session` block".into(),
            }
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
            let scope = match &need.scope {
                ScopeBinding::FromArg { arg } => {
                    let val = args.get(arg).ok_or_else(|| {
                        ManifestError::SessionNeedInvalid {
                            tool: tool_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` not supplied at call time"),
                        }
                    })?;
                    let arg_decl = tool
                        .args
                        .iter()
                        .find(|a| a.name == *arg)
                        .ok_or_else(|| ManifestError::SessionNeedRefsUndeclaredArg {
                            tool: tool_name.to_string(),
                            idx,
                            arg: arg.clone(),
                        })?;
                    scope_from_arg_value(arg_decl.kind, val).ok_or_else(|| {
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
                    let value = args
                        .get(arg)
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| ManifestError::SessionNeedInvalid {
                            tool: tool_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` must be a string"),
                        })?;
                    values.get(value).cloned().ok_or_else(|| {
                        ManifestError::SessionNeedInvalid {
                            tool: tool_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` has unmapped value `{value}`"),
                        }
                    })?
                }
                ScopeBinding::FromArgOrWild { arg, wild_when } => {
                    if args
                        .get(wild_when)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        Scope::Wild
                    } else {
                        let value = args.get(arg).ok_or_else(|| {
                            ManifestError::SessionNeedInvalid {
                                tool: tool_name.to_string(),
                                idx,
                                detail: format!("arg `{arg}` not supplied at call time"),
                            }
                        })?;
                        let decl = tool
                            .args
                            .iter()
                            .find(|decl| decl.name == *arg)
                            .ok_or_else(|| {
                                ManifestError::SessionNeedRefsUndeclaredArg {
                                    tool: tool_name.to_string(),
                                    idx,
                                    arg: arg.clone(),
                                }
                            })?;
                        scope_from_arg_value(decl.kind, value).ok_or_else(|| {
                            ManifestError::SessionNeedInvalid {
                                tool: tool_name.to_string(),
                                idx,
                                detail: format!("arg `{arg}` cannot populate a scope"),
                            }
                        })?
                    }
                }
                ScopeBinding::Fixed { scope } => scope.clone(),
                ScopeBinding::Wild => Scope::Wild,
            };
            out.push(super::cap::Cap::new(need.verb, scope));
        }
        Ok(out)
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
    use super::*;

    fn parse(s: &str) -> Manifest {
        Manifest::from_json(s).expect("manifest should be valid")
    }

    #[test]
    fn minimal_manifest_parses() {
        let m = parse(
            r#"{
              "id": "fs",
              "version": "0.2.0",
              "name": "Files"
            }"#,
        );
        assert_eq!(m.id, "fs");
        assert_eq!(m.runtime, Runtime::Python);
        assert!(m.operations.is_empty());
    }

    #[test]
    fn invalid_id_rejected() {
        let err = Manifest::from_json(
            r#"{"id":"FS!","version":"0","name":"X"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::InvalidId(_)));
    }

    #[test]
    fn unknown_verb_rejected_at_parse_time() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "x": {
                  "label": "X",
                  "args": [],
                  "needs": [
                    {"verb": "fs.nonsense", "scope": {"kind":"wild"}, "why": "..."}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        // Serde error, not validate(): the unknown verb is caught at
        // deserialization time by Verb's manual impl.
        assert!(matches!(err, ManifestError::Json(_)));
    }

    #[test]
    fn need_referencing_undeclared_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": "y"}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        match err {
            ManifestError::NeedRefsUndeclaredArg { op, idx, arg } => {
                assert_eq!(op, "rm");
                assert_eq!(idx, 0);
                assert_eq!(arg, "path");
            }
            other => panic!("expected NeedRefsUndeclaredArg, got {other:?}"),
        }
    }

    #[test]
    fn need_binding_to_text_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "text"}],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": "y"}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::NeedArgKindMismatch { .. }));
    }

    #[test]
    fn duplicate_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "x": {
                  "label": "X",
                  "args": [
                    {"name": "p", "kind": "path"},
                    {"name": "p", "kind": "path"}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateArg { .. }));
    }

    #[test]
    fn missing_english_in_top_level_name_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"zh-CN": "文件"}
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::TopLevelTextInvalid { field: "name", .. }));
    }

    #[test]
    fn missing_english_in_op_label_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "ls": {
                  "label": {"zh-CN": "列表"}
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::LocalizedTextInvalid { field: "label", .. }
        ));
    }

    #[test]
    fn resolve_needs_substitutes_runtime_arg_value() {
        let m = parse(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Remove the file you specified."}
                  ]
                }
              }
            }"#,
        );
        let mut args = BTreeMap::new();
        args.insert("path".to_string(), serde_json::json!("/home/jay/x.md"));
        let caps = m.resolve_needs("rm", &args).unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].verb, Verb::FS_DELETE);
        assert_eq!(caps[0].scope, Scope::path("/home/jay/x.md"));
    }

    #[test]
    fn resolve_needs_with_fixed_scope() {
        let m = parse(
            r#"{
              "id": "log",
              "version": "0.1",
              "name": "Log",
              "operations": {
                "tail": {
                  "label": "Tail logs",
                  "needs": [
                    {"verb": "data.log.read",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"system/*"}},
                     "why": "Read recent log lines."}
                  ]
                }
              }
            }"#,
        );
        let caps = m.resolve_needs("tail", &BTreeMap::new()).unwrap();
        assert_eq!(caps[0].verb, Verb::DATA_LOG_READ);
        assert_eq!(caps[0].scope, Scope::name("system/*"));
    }

    #[test]
    fn resolve_needs_missing_arg_at_runtime_is_error() {
        let m = parse(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Remove the file you specified."}
                  ]
                }
              }
            }"#,
        );
        let err = m.resolve_needs("rm", &BTreeMap::new()).unwrap_err();
        match err {
            ManifestError::NeedInvalid { op, detail, .. } => {
                assert_eq!(op, "rm");
                assert!(detail.contains("not supplied"));
            }
            other => panic!("expected NeedInvalid, got {other:?}"),
        }
    }

    #[test]
    fn runtime_default_is_python() {
        let m = parse(
            r#"{"id":"x","version":"0","name":"X"}"#,
        );
        assert_eq!(m.runtime, Runtime::Python);
        assert_eq!(m.runtime.default_entry(), "main.py");
    }

    #[test]
    fn full_example_round_trips() {
        let src = r#"{
          "id": "fs",
          "version": "0.2.0",
          "name": "Files",
          "summary": "Browse, read, write, and search files.",
          "icon": "📁",
          "runtime": "python",
          "entry": "main.py",
          "operations": {
            "ls": {
              "label": "List files",
              "summary": "Show the names of files inside a folder.",
              "args": [{"name":"path","kind":"path","required":true}],
              "needs": [
                {"verb":"fs.meta",
                 "scope":{"kind":"from-arg","arg":"path"},
                 "why":"Read directory entries to list files."}
              ]
            },
            "mv": {
              "label": "Move a file",
              "args": [
                {"name":"src","kind":"path","required":true},
                {"name":"dst","kind":"path","required":true}
              ],
              "needs": [
                {"verb":"fs.read",   "scope":{"kind":"from-arg","arg":"src"}, "why":"Read the source file."},
                {"verb":"fs.write",  "scope":{"kind":"from-arg","arg":"dst"}, "why":"Write to the destination."},
                {"verb":"fs.delete", "scope":{"kind":"from-arg","arg":"src"}, "why":"Remove the source after copying."}
              ]
            }
          }
        }"#;
        let m = Manifest::from_json(src).unwrap();
        let json = serde_json::to_string(&m).unwrap();
        let back = Manifest::from_json(&json).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.operations.len(), m.operations.len());
        assert_eq!(back.operations["mv"].needs.len(), 3);
    }

    // ---------------------------------------------------------------
    // AI policy block
    // ---------------------------------------------------------------

    #[test]
    fn ai_block_with_valid_policy_parses() {
        let m = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 100000},
                "safety": "strict",
                "origins": ["external-content"]
              },
              "operations": {
                "run": {
                  "label": "Summarize text",
                  "needs": [
                    {"verb": "ai.chat.untrusted",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": "Summarize the input text."}
                  ]
                }
              }
            }"#,
        )
        .unwrap();
        let policy = m.ai.as_ref().unwrap();
        assert_eq!(policy.safety, AiSafety::Strict);
        assert_eq!(policy.origins, vec![PromptOrigin::ExternalContent]);
        assert_eq!(policy.budget.monthly_units, 100000);
    }

    #[test]
    fn ai_need_without_ai_block_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "rogue",
              "version": "0.1",
              "name": "Rogue",
              "operations": {
                "run": {
                  "label": "Run",
                  "needs": [
                    {"verb": "ai.chat",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": "Talk to a model without declaring a policy."}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        match err {
            ManifestError::AiNeedMissingPolicy { op, verb, .. } => {
                assert_eq!(op, "run");
                assert_eq!(verb, "ai.chat");
            }
            other => panic!("expected AiNeedMissingPolicy, got {other:?}"),
        }
    }

    #[test]
    fn ai_bypass_rejected_for_apps() {
        let err = Manifest::from_json(
            r#"{
              "id": "rogue",
              "version": "0.1",
              "name": "Rogue",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "minimal",
                "origins": ["trusted"]
              },
              "operations": {
                "run": {
                  "label": "Run",
                  "needs": [
                    {"verb": "ai.bypass",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": "Skip safety pipeline."}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::AiBypassNotAllowedForApps { .. }));
    }

    #[test]
    fn ai_block_with_empty_origins_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": []
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::AiPolicyNoOrigins));
    }

    #[test]
    fn ai_origins_default_to_trusted() {
        let m = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict"
              }
            }"#,
        )
        .unwrap();
        let policy = m.ai.as_ref().unwrap();
        assert_eq!(policy.origins, vec![PromptOrigin::Trusted]);
    }

    #[test]
    fn ai_tools_default_to_empty_list() {
        let m = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"]
              }
            }"#,
        )
        .unwrap();
        let policy = m.ai.as_ref().unwrap();
        assert!(policy.tools.is_empty());
    }

    #[test]
    fn ai_tools_duplicate_entry_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text", "kv.get", "fs.read_text"]
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::AiDuplicateTool { ref name } if name == "fs.read_text"));
    }

    #[test]
    fn ai_tools_unknown_name_rejected_against_catalog() {
        let m = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text", "fs.unicorn"]
              }
            }"#,
        )
        .unwrap();
        let err = m
            .validate_tools_against_catalog(&["fs.read_text", "kv.get"])
            .unwrap_err();
        assert!(matches!(err, ManifestError::AiUnknownTool { ref name } if name == "fs.unicorn"));
    }

    #[test]
    fn ai_tools_known_names_pass_catalog_check() {
        let m = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text", "kv.get"]
              }
            }"#,
        )
        .unwrap();
        assert!(m
            .validate_tools_against_catalog(&["fs.read_text", "fs.list", "kv.get"])
            .is_ok());
    }

    #[test]
    fn manifest_without_ai_block_skips_tool_catalog_check() {
        let m = Manifest::from_json(
            r#"{
              "id": "calc",
              "version": "0.1",
              "name": "Calc"
            }"#,
        )
        .unwrap();
        assert!(m.validate_tools_against_catalog(&[]).is_ok());
    }

    // -----------------------------------------------------------------
    // Session block tests (Phase 11)
    // -----------------------------------------------------------------

    #[test]
    fn session_block_parses_with_minimal_tool() {
        let m = parse(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "entry": "server.py",
                "tools": [
                  {
                    "name": "kv.list",
                    "summary": "List keys.",
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"wild"},
                       "why": "Scan every key."}
                    ]
                  }
                ]
              }
            }"#,
        );
        let session = m.session.expect("session block parsed");
        assert_eq!(session.entry.as_deref(), Some("server.py"));
        assert_eq!(session.transport, SessionTransport::Stdio);
        assert_eq!(session.tools.len(), 1);
        assert_eq!(session.tools[0].name, "kv.list");
    }

    #[test]
    fn session_tool_default_entry_per_runtime() {
        assert_eq!(Runtime::Python.default_session_entry(), "server.py");
        assert_eq!(Runtime::Node.default_session_entry(), "server.js");
    }

    #[test]
    fn session_tool_resolve_needs_from_arg() {
        let m = parse(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": "Get a value.",
                    "args": [{"name":"key","kind":"name","required":true}],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": "Read the value at the named key."}
                    ]
                  }
                ]
              }
            }"#,
        );
        let mut args = BTreeMap::new();
        args.insert("key".to_string(), serde_json::json!("user/jay"));
        let caps = m.resolve_session_tool_needs("kv.get", &args).unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].verb, Verb::DATA_KV_READ);
        assert_eq!(caps[0].scope, Scope::name("user/jay"));
    }

    #[test]
    fn session_tool_resolve_needs_unknown_tool_errors() {
        let m = parse(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": { "tools": [] }
            }"#,
        );
        let err = m
            .resolve_session_tool_needs("kv.ghost", &BTreeMap::new())
            .unwrap_err();
        assert!(matches!(err, ManifestError::SessionNeedInvalid { .. }));
    }

    #[test]
    fn session_tool_resolve_needs_no_session_errors() {
        let m = parse(
            r#"{"id":"kv","version":"0","name":"KV"}"#,
        );
        let err = m
            .resolve_session_tool_needs("kv.get", &BTreeMap::new())
            .unwrap_err();
        match err {
            ManifestError::SessionNeedInvalid { detail, .. } => {
                assert!(detail.contains("no `session` block"));
            }
            other => panic!("expected SessionNeedInvalid, got {other:?}"),
        }
    }

    #[test]
    fn session_tool_invalid_name_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {"name": "KV.Get", "summary": "Get"}
                ]
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::SessionToolInvalidName { .. }));
    }

    #[test]
    fn session_duplicate_tool_name_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {"name": "kv.get", "summary": "Get"},
                  {"name": "kv.get", "summary": "Get again"}
                ]
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::SessionDuplicateTool { .. }));
    }

    #[test]
    fn session_need_refs_undeclared_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": "Get",
                    "args": [],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": "Read."}
                    ]
                  }
                ]
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::SessionNeedRefsUndeclaredArg { .. }
        ));
    }

    #[test]
    fn session_need_binding_to_text_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": "Get",
                    "args": [{"name":"key","kind":"text"}],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": "Read."}
                    ]
                  }
                ]
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::SessionNeedArgKindMismatch { .. }
        ));
    }

    #[test]
    fn session_ai_verb_without_policy_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "session": {
                "tools": [
                  {
                    "name": "summarize.run",
                    "summary": "Summarize text.",
                    "needs": [
                      {"verb": "ai.chat",
                       "scope": {"kind":"wild"},
                       "why": "Call the model."}
                    ]
                  }
                ]
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::SessionAiNeedMissingPolicy { .. }
        ));
    }

    #[test]
    fn desktop_block_parses_with_defaults() {
        let m = parse(
            r#"{
              "id": "notes",
              "version": "0.1",
              "name": "Notes",
              "desktop": {}
            }"#,
        );
        let d = m.desktop.expect("desktop block present");
        assert_eq!(d.exec, "--gui");
        assert!(!d.single_instance);
        assert!(d.categories.is_empty());
    }

    #[test]
    fn desktop_block_full_parses() {
        let m = parse(
            r#"{
              "id": "notes",
              "version": "0.1",
              "name": "Notes",
              "desktop": {
                "exec": "--ui",
                "name": "My Notes",
                "icon": "notes",
                "categories": ["Utility", "TextEditor"],
                "mime_types": ["text/markdown"],
                "single_instance": true
              }
            }"#,
        );
        let d = m.desktop.expect("desktop block present");
        assert_eq!(d.exec, "--ui");
        assert_eq!(d.name.unwrap().en_str(), "My Notes");
        assert_eq!(d.categories, vec!["Utility", "TextEditor"]);
        assert_eq!(d.mime_types, vec!["text/markdown"]);
        assert!(d.single_instance);
    }

    #[test]
    fn desktop_rejects_category_with_separator() {
        let err = Manifest::from_json(
            r#"{
              "id": "notes",
              "version": "0.1",
              "name": "Notes",
              "desktop": { "categories": ["Utility;Evil"] }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::DesktopInvalid { field: "categories", .. }
        ));
    }

    #[test]
    fn desktop_rejects_control_char_in_exec() {
        let err = Manifest::from_json(
            "{\"id\":\"notes\",\"version\":\"0.1\",\"name\":\"Notes\",\
             \"desktop\":{\"exec\":\"--gui\\nExec=evil\"}}",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::DesktopInvalid { field: "exec", .. }
        ));
    }
}

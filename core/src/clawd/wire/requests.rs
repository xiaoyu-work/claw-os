//! Typed request bodies — one per broker route.
//!
//! These are the broker boundary. A route's parameters stop being
//! `serde_json::Value` here: every struct is `deny_unknown_fields`, so
//! a field the route never declared is a decode failure *before*
//! authorization, and every field is one of the bounded types in
//! [`super::bounded`], so a value that is too long, too deep or the
//! wrong JSON type is refused before any handler sees it.
//!
//! A handful of fields stay [`Structured`] because their shape is the
//! route's public contract and the owning authority — not the broker —
//! is what validates them: a canonical [`crate::caps::Scope`], a
//! serialized [`crate::caps::CapSet`], an App session tool call, a
//! scheduler argument vector, a context source document. Those are
//! still size-bounded here, and the containing request is still typed
//! and closed.
//!
//! After decoding, the registry re-serializes the struct back into the
//! canonical object the handler reads. Nothing survives that round trip
//! except fields this module declared, so a handler cannot reach a
//! value the boundary did not validate.

use serde::{Deserialize, Serialize};

use super::bounded::{Name, NoParams, Structured, Text, TextList, Token, WaitMillis};

/// Free-text ceilings. A request frame is capped well below the sum of
/// these, so they bound one field rather than the message.
const PROMPT_BYTES: usize = 512 * 1024;
const PATH_BYTES: usize = 4096;
const LABEL_BYTES: usize = 1024;
const COMMAND_BYTES: usize = 8192;

pub type NoBody = NoParams;

// ---------------------------------------------------------------------------
// Agent tasks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSubmit {
    pub prompt: Text<PROMPT_BYTES>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Text<PROMPT_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<Text<PROMPT_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskId {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWait {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<WaitMillis>,
}

// ---------------------------------------------------------------------------
// Memory, context and journals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryHistory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySessions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOperations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextUpdate {
    pub source: Token,
    /// The collector document itself. Shape belongs to the source, not
    /// to the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Structured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEventAppend {
    pub event_type: Token,
    pub source: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<Token>,
    /// Event body. Producers own the schema; the broker only bounds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Structured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEventQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionBegin {
    pub purpose: Text<LABEL_BYTES>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionId {
    pub id: Token,
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionStatus {
    pub ids: TextList<64, 128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRequest {
    pub verb: Name,
    /// A canonical [`crate::caps::Scope`]; `permissions` parses it into
    /// the typed scope model before anything is filed.
    pub scope: Structured,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Token>,
    pub reason: Text<LABEL_BYTES>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionDecide {
    pub id: Token,
    pub decision: Token,
    /// Honoured only for a root peer — the privileged approval helper
    /// naming the desktop user it authenticated. A non-root peer is
    /// refused by the route before this is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Text<LABEL_BYTES>>,
}

/// Retire reusable approvals for an owner or one of their grant
/// sessions.
///
/// Root-only at the route's access class, because `owner_uid` names
/// whose authority is being retired and a non-root peer must not be
/// able to choose another account — in either direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRevoke {
    /// The account whose approvals are retired. Absent means the
    /// unattributed, system-scoped ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,
    /// One grant session. Absent retires everything the owner holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Token>,
}

// ---------------------------------------------------------------------------
// App / MCP sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionRegister {
    pub app_id: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<TextList<128, PATH_BYTES>>,
    /// The launcher's own capability set, used only to narrow what the
    /// daemon already resolved for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_caps: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionRegisterNative {
    pub app_id: Name,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSessionRegister {
    pub command: Text<COMMAND_BYTES>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_caps: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionBind {
    pub session_id: Token,
    pub handle: Token,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionSetTransient {
    pub session_id: Token,
    pub handle: Token,
    /// One MCP session tool invocation, validated against the installed
    /// manifest by the App session authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<Structured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_caps: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionDeregister {
    pub session_id: Token,
    pub handle: Token,
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerRun {
    pub subsystem: Token,
    pub command: Token,
    /// The `cos cron` / `cos triggers` argument vector. `scheduler`
    /// re-validates it against the allow-listed command before the job
    /// or rule id it addresses is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Structured>,
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialOauthRefresh {
    pub session: Token,
    pub namespace: Name,
    pub credential: Name,
}

// ---------------------------------------------------------------------------
// System services
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Text<LABEL_BYTES>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_daily: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_weekly: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_monthly: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BluetoothControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_serial: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashInspect {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_sync: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlight: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirewallControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_action: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAction {
    pub session: Token,
    pub action: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationQuery {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageInstall {
    pub session: Token,
    /// Overwritten with `install` by the route before dispatch; kept
    /// declarable so an existing caller that sends it is not refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRestore {
    pub session: Token,
    pub mutation_session: Token,
    pub mutation_seq: u64,
    pub package: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<Name>,
    pub was_held: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrinterControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printer: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copies: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRestore {
    pub session: Token,
    pub mutation_session: Token,
    pub mutation_seq: u64,
    pub unit: Name,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsersControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/wire/requests.rs"
    ));
}

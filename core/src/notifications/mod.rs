//! Durable user-attention notifications.
//!
//! Producers publish bounded facts through [`NotificationService`]. The
//! SQLite provider persists them before any channel attempts delivery, while
//! Web, desktop, and external adapters consume owner-scoped change streams or
//! delivery leases.

mod ntfy;
mod sqlite;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use ntfy::{DeliveryAdapter, DeliveryError, NtfyAdapter, NtfyTarget};
pub use sqlite::SqliteNotificationService;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_TITLE_CHARS: usize = 240;
pub const MAX_BODY_CHARS: usize = 4_000;
pub const MAX_ACTIONS: usize = 4;
pub const DEFAULT_LIST_LIMIT: usize = 100;
pub const MAX_LIST_LIMIT: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    #[error("invalid notification: {0}")]
    Invalid(String),
    #[error("notification not found")]
    NotFound,
    #[error("notification database is unavailable: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("notification storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("notification serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("notification database lock is poisoned")]
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }

    pub fn parse(value: &str) -> Result<Self, NotificationError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "info" => Ok(Self::Info),
            "warning" | "warn" => Ok(Self::Warning),
            "error" => Ok(Self::Error),
            "critical" => Ok(Self::Critical),
            _ => Err(NotificationError::Invalid(
                "severity must be info, warning, error, or critical".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPolicy {
    /// Persist in activity views without interrupting the user.
    Activity,
    /// Deliver immediately to every enabled channel allowed by policy.
    Immediate,
}

impl DeliveryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Immediate => "immediate",
        }
    }

    pub fn parse(value: &str) -> Result<Self, NotificationError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "activity" | "silent" => Ok(Self::Activity),
            "immediate" => Ok(Self::Immediate),
            _ => Err(NotificationError::Invalid(
                "delivery policy must be activity or immediate".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryChannel {
    Web,
    Desktop,
    Ntfy,
}

impl DeliveryChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Desktop => "desktop",
            Self::Ntfy => "ntfy",
        }
    }

    pub fn parse(value: &str) -> Result<Self, NotificationError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "web" => Ok(Self::Web),
            "desktop" => Ok(Self::Desktop),
            "ntfy" => Ok(Self::Ntfy),
            _ => Err(NotificationError::Invalid(
                "delivery channel must be web, desktop, or ntfy".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Delivering,
    Delivered,
    Failed,
    Suppressed,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivering => "delivering",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Suppressed => "suppressed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, NotificationError> {
        match value {
            "queued" => Ok(Self::Queued),
            "delivering" => Ok(Self::Delivering),
            "delivered" => Ok(Self::Delivered),
            "failed" => Ok(Self::Failed),
            "suppressed" => Ok(Self::Suppressed),
            _ => Err(NotificationError::Invalid(
                "stored delivery state is invalid".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationState {
    Unread,
    Read,
    Acknowledged,
    Dismissed,
}

impl NotificationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Read => "read",
            Self::Acknowledged => "acknowledged",
            Self::Dismissed => "dismissed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, NotificationError> {
        match value {
            "unread" => Ok(Self::Unread),
            "read" => Ok(Self::Read),
            "acknowledged" => Ok(Self::Acknowledged),
            "dismissed" => Ok(Self::Dismissed),
            _ => Err(NotificationError::Invalid(
                "stored notification state is invalid".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationAction {
    pub id: String,
    pub label: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationDraft {
    pub source: String,
    pub kind: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub delivery_policy: DeliveryPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub actions: Vec<NotificationAction>,
}

impl NotificationDraft {
    pub fn new(
        source: impl Into<String>,
        kind: impl Into<String>,
        severity: Severity,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            kind: kind.into(),
            severity,
            title: title.into(),
            body: body.into(),
            delivery_policy: DeliveryPolicy::Immediate,
            dedupe_key: None,
            task_id: None,
            session_id: None,
            job_id: None,
            expires_at_ms: None,
            actions: Vec::new(),
        }
    }

    pub fn activity(mut self) -> Self {
        self.delivery_policy = DeliveryPolicy::Activity;
        self
    }

    pub fn dedupe(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    pub fn validate(&self) -> Result<(), NotificationError> {
        validate_identifier("source", &self.source, 128)?;
        validate_identifier("kind", &self.kind, 128)?;
        validate_text("title", &self.title, MAX_TITLE_CHARS, false)?;
        validate_text("body", &self.body, MAX_BODY_CHARS, true)?;
        if let Some(key) = &self.dedupe_key {
            validate_identifier("dedupe_key", key, 192)?;
        }
        for (name, value) in [
            ("task_id", self.task_id.as_deref()),
            ("session_id", self.session_id.as_deref()),
            ("job_id", self.job_id.as_deref()),
        ] {
            if let Some(value) = value {
                validate_identifier(name, value, 192)?;
            }
        }
        if self.actions.len() > MAX_ACTIONS {
            return Err(NotificationError::Invalid(format!(
                "actions exceeds the maximum of {MAX_ACTIONS}"
            )));
        }
        for action in &self.actions {
            validate_identifier("action id", &action.id, 64)?;
            validate_text("action label", &action.label, 80, false)?;
            validate_text("action uri", &action.uri, 1_024, false)?;
            let uri = url::Url::parse(&action.uri).map_err(|_| {
                NotificationError::Invalid("action uri must be an absolute URL".to_string())
            })?;
            if !matches!(uri.scheme(), "clawos" | "https") {
                return Err(NotificationError::Invalid(
                    "action uri scheme must be clawos or https".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryStatus {
    pub channel: DeliveryChannel,
    pub state: DeliveryState,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub schema: u32,
    pub sequence: u64,
    pub id: String,
    pub owner_uid: u32,
    pub source: String,
    pub kind: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub delivery_policy: DeliveryPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub state: NotificationState,
    pub occurrences: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at_ms: Option<i64>,
    #[serde(default)]
    pub actions: Vec<NotificationAction>,
    #[serde(default)]
    pub deliveries: Vec<DeliveryStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationChange {
    pub cursor: u64,
    pub change: String,
    pub notification: Notification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeBatch {
    pub cursor: u64,
    pub changes: Vec<NotificationChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub web_enabled: bool,
    pub desktop_enabled: bool,
    pub ntfy_enabled: bool,
    pub web_min_severity: Severity,
    pub desktop_min_severity: Severity,
    pub ntfy_min_severity: Severity,
    #[serde(default)]
    pub muted_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dnd_start_minute_utc: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dnd_end_minute_utc: Option<u16>,
    pub critical_bypasses_dnd: bool,
    pub retention_days: u16,
    pub ntfy_server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ntfy_topic: Option<String>,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            web_enabled: true,
            desktop_enabled: true,
            ntfy_enabled: false,
            web_min_severity: Severity::Info,
            desktop_min_severity: Severity::Info,
            ntfy_min_severity: Severity::Warning,
            muted_kinds: Vec::new(),
            dnd_start_minute_utc: None,
            dnd_end_minute_utc: None,
            critical_bypasses_dnd: true,
            retention_days: 30,
            ntfy_server: "https://ntfy.sh".to_string(),
            ntfy_topic: None,
        }
    }
}

impl NotificationPreferences {
    pub fn validate(&self) -> Result<(), NotificationError> {
        match (self.dnd_start_minute_utc, self.dnd_end_minute_utc) {
            (None, None) => {}
            (Some(start), Some(end)) if start < 1_440 && end < 1_440 => {}
            _ => {
                return Err(NotificationError::Invalid(
                    "DND start and end must both be UTC minutes in 0..1440".to_string(),
                ));
            }
        }
        if !(1..=365).contains(&self.retention_days) {
            return Err(NotificationError::Invalid(
                "retention_days must be in 1..=365".to_string(),
            ));
        }
        for kind in &self.muted_kinds {
            validate_identifier("muted notification kind", kind, 128)?;
        }
        let server = url::Url::parse(&self.ntfy_server)
            .map_err(|_| NotificationError::Invalid("ntfy_server must be a URL".to_string()))?;
        if server.scheme() != "https"
            || server.host_str() != Some("ntfy.sh")
            || !server.username().is_empty()
            || server.password().is_some()
            || server.query().is_some()
            || server.fragment().is_some()
        {
            return Err(NotificationError::Invalid(
                "ntfy_server must use the trusted https://ntfy.sh origin".to_string(),
            ));
        }
        if let Some(topic) = &self.ntfy_topic {
            if topic.is_empty()
                || topic.len() > 192
                || !topic.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@' | b'+')
                })
            {
                return Err(NotificationError::Invalid(
                    "ntfy_topic contains unsupported characters".to_string(),
                ));
            }
        }
        if self.ntfy_enabled && self.ntfy_topic.is_none() {
            return Err(NotificationError::Invalid(
                "ntfy_topic is required when ntfy delivery is enabled".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationMutation {
    Read,
    Acknowledge,
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryClaim {
    pub notification: Notification,
    pub channel: DeliveryChannel,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryResult {
    Delivered,
    Suppressed,
    Failed {
        error_code: String,
        retry_at_ms: i64,
    },
}

pub trait NotificationService: Send + Sync {
    fn publish(
        &self,
        owner_uid: u32,
        draft: NotificationDraft,
    ) -> Result<Notification, NotificationError>;

    fn list(
        &self,
        owner_uid: u32,
        include_dismissed: bool,
        limit: usize,
    ) -> Result<Vec<Notification>, NotificationError>;

    fn changes(
        &self,
        owner_uid: u32,
        after_cursor: u64,
        limit: usize,
    ) -> Result<ChangeBatch, NotificationError>;

    fn cursor(&self, owner_uid: u32) -> Result<u64, NotificationError>;

    fn mutate(
        &self,
        owner_uid: u32,
        id: &str,
        mutation: NotificationMutation,
    ) -> Result<Notification, NotificationError>;

    fn preferences(&self, owner_uid: u32) -> Result<NotificationPreferences, NotificationError>;

    fn set_preferences(
        &self,
        owner_uid: u32,
        preferences: NotificationPreferences,
    ) -> Result<NotificationPreferences, NotificationError>;

    fn claim_deliveries(
        &self,
        owner_uid: Option<u32>,
        channel: DeliveryChannel,
        limit: usize,
        lease_ms: i64,
    ) -> Result<Vec<DeliveryClaim>, NotificationError>;

    fn complete_delivery(
        &self,
        owner_uid: u32,
        id: &str,
        channel: DeliveryChannel,
        result: DeliveryResult,
    ) -> Result<Notification, NotificationError>;

    fn known_owner_uids(&self) -> Result<Vec<u32>, NotificationError>;
}

pub fn open_default() -> Result<SqliteNotificationService, NotificationError> {
    SqliteNotificationService::open(crate::paths::notifications_db_path())
}

pub fn open(path: impl AsRef<Path>) -> Result<SqliteNotificationService, NotificationError> {
    SqliteNotificationService::open(path)
}

pub fn bounded_body(value: &str) -> String {
    if value.chars().count() <= MAX_BODY_CHARS {
        return value.to_string();
    }
    let mut body: String = value.chars().take(MAX_BODY_CHARS - 3).collect();
    body.push_str("...");
    body
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn validate_identifier(field: &str, value: &str, max: usize) -> Result<(), NotificationError> {
    if value.is_empty() || value.len() > max {
        return Err(NotificationError::Invalid(format!(
            "{field} must contain 1..={max} bytes"
        )));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/' | b'@' | b'+')
    }) {
        return Err(NotificationError::Invalid(format!(
            "{field} contains unsupported characters"
        )));
    }
    Ok(())
}

fn validate_text(
    field: &str,
    value: &str,
    max_chars: usize,
    allow_newlines: bool,
) -> Result<(), NotificationError> {
    let count = value.chars().count();
    if value.trim().is_empty() || count > max_chars {
        return Err(NotificationError::Invalid(format!(
            "{field} must contain 1..={max_chars} characters"
        )));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !(allow_newlines && matches!(ch, '\n' | '\r' | '\t')))
    {
        return Err(NotificationError::Invalid(format!(
            "{field} contains unsupported control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/notifications/mod.rs"
    ));
}

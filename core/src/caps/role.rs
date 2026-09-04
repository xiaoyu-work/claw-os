//! Role bundles — human-friendly names for common cap sets.
//!
//! A [`Role`] is **purely a UX shortcut**: it expands to a list of
//! capability *verbs* that the kernel pairs with the user-supplied
//! scope to form concrete caps. The kernel never stores "role" — it
//! stores [`CapSet`](super::cap::CapSet). Roles only exist where users
//! pick from a menu.
//!
//! This keeps the privilege model coherent (one primitive: `Cap`)
//! while letting non-technical users say "let this agent be a `worker`"
//! and not have to think about thirty-something verbs.
//!
//! ## Built-in roles
//!
//! | Role          | Label                  | What it can do                                                 |
//! |---------------|------------------------|----------------------------------------------------------------|
//! | `observer`    | Watcher                | Read files & metadata, list processes, receive notifications.  |
//! | `worker`      | Worker                 | + write files & data, ask the user, publish to channels.       |
//! | `curator`     | Organizer              | + delete files (kept in trash for 30 days).                    |
//! | `connector`   | Researcher             | Observer + outbound network + read named secrets.              |
//! | `automator`   | Automator              | Curator + run programs + outbound network + secrets.           |
//! | `agent-host`  | Agent Coordinator      | Automator + spawn / call / delegate to sub-agents.             |
//! | `admin`       | Administrator          | Agent-host + system services + packages + devices.             |
//! | `kernel`      | Kernel                 | Reserved: held only by the cos process itself.                  |
//!
//! Roles are intentionally only verb sets — pairing with a scope is
//! the caller's responsibility (see [`Role::caps_with_scopes`]). This
//! prevents nasty surprises like a "worker" granted at `/` instead of
//! the user-confirmed folder.

use crate::i18n::LocalizedStr;

use super::cap::{Cap, CapSet};
use super::scope::Scope;
use super::verb::Verb;

/// One of the built-in role bundles.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Observer,
    Worker,
    Curator,
    Connector,
    Automator,
    AgentHost,
    Admin,
    Kernel,
}

impl Role {
    /// Canonical kebab-case identifier used in CLI and config files.
    pub fn name(self) -> &'static str {
        match self {
            Role::Observer => "observer",
            Role::Worker => "worker",
            Role::Curator => "curator",
            Role::Connector => "connector",
            Role::Automator => "automator",
            Role::AgentHost => "agent-host",
            Role::Admin => "admin",
            Role::Kernel => "kernel",
        }
    }

    /// Short user-facing label.
    pub fn label(self) -> LocalizedStr {
        match self {
            Role::Observer => LocalizedStr::new("Watcher"),
            Role::Worker => LocalizedStr::new("Worker"),
            Role::Curator => LocalizedStr::new("Organizer"),
            Role::Connector => LocalizedStr::new("Researcher"),
            Role::Automator => LocalizedStr::new("Automator"),
            Role::AgentHost => LocalizedStr::new("Agent Coordinator"),
            Role::Admin => LocalizedStr::new("Administrator"),
            Role::Kernel => LocalizedStr::new("Kernel (system only)"),
        }
    }

    /// One-line elaboration shown next to the label in role pickers.
    pub fn blurb(self) -> LocalizedStr {
        match self {
            Role::Observer => LocalizedStr::new(
                "Reads files and watches for changes. Cannot modify, delete, or go online.",
            ),
            Role::Worker => LocalizedStr::new(
                "Reads and writes files in the granted folder. Cannot delete or use the network.",
            ),
            Role::Curator => LocalizedStr::new(
                "Worker plus the ability to delete files (kept recoverable for 30 days).",
            ),
            Role::Connector => LocalizedStr::new(
                "Reads files and reaches the listed websites. Cannot modify your computer.",
            ),
            Role::Automator => LocalizedStr::new(
                "Reads, writes, deletes, runs programs, and reaches the network. The typical assistant.",
            ),
            Role::AgentHost => LocalizedStr::new(
                "Automator plus the ability to spawn helper agents and share permissions with them.",
            ),
            Role::Admin => LocalizedStr::new(
                "Almost full control of the computer, including installing software and managing services.",
            ),
            Role::Kernel => LocalizedStr::new(
                "Reserved for the operating system itself. Not assignable to user agents.",
            ),
        }
    }

    /// Parse the canonical CLI name (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "observer" => Some(Role::Observer),
            "worker" => Some(Role::Worker),
            "curator" => Some(Role::Curator),
            "connector" => Some(Role::Connector),
            "automator" => Some(Role::Automator),
            "agent-host" | "agent_host" | "host" => Some(Role::AgentHost),
            "admin" => Some(Role::Admin),
            "kernel" => Some(Role::Kernel),
            _ => None,
        }
    }

    /// Legacy credential tier corresponding to this capability role.
    /// Lower numbers are more privileged.
    pub fn credential_tier(self) -> u8 {
        match self {
            Role::Kernel | Role::Admin => 0,
            Role::AgentHost | Role::Automator => 1,
            Role::Worker | Role::Curator | Role::Connector => 2,
            Role::Observer => 3,
        }
    }

    /// Verbs included in this role. Combine with scopes to form a
    /// concrete [`CapSet`].
    pub fn verbs(self) -> &'static [Verb] {
        match self {
            Role::Observer => OBSERVER_VERBS,
            Role::Worker => WORKER_VERBS,
            Role::Curator => CURATOR_VERBS,
            Role::Connector => CONNECTOR_VERBS,
            Role::Automator => AUTOMATOR_VERBS,
            Role::AgentHost => AGENT_HOST_VERBS,
            Role::Admin => ADMIN_VERBS,
            Role::Kernel => KERNEL_VERBS,
        }
    }

    /// Build a concrete [`CapSet`] by pairing each verb with the
    /// appropriate user-supplied scope.
    ///
    /// `path_scope` bounds capabilities cataloged with `ScopeKind::Path`;
    /// `host_scope` bounds capabilities cataloged with `ScopeKind::Host`;
    /// `name_scope` bounds capabilities cataloged with `ScopeKind::Name` or
    /// `ScopeKind::SelfRef`, including category-scoped `net.manage`;
    /// unscoped verbs (ui.*, time.*) take [`Scope::Wild`].
    ///
    /// If a scope is `None` for a kind the role would have used, those
    /// caps are dropped — i.e. asking for a connector with `host_scope:
    /// None` yields the observer subset only, never a more permissive
    /// fallback.
    pub fn caps_with_scopes(
        self,
        path_scope: Option<Scope>,
        host_scope: Option<Scope>,
        name_scope: Option<Scope>,
    ) -> CapSet {
        use super::scope::ScopeKind;
        let meta_for = super::catalog::lookup;

        let mut set = CapSet::new();
        for &verb in self.verbs() {
            let kind = meta_for(verb)
                .map(|m| m.scope_kind)
                .unwrap_or(ScopeKind::Wild);
            let scope = match kind {
                ScopeKind::Path => path_scope.clone(),
                ScopeKind::Host => host_scope.clone(),
                ScopeKind::Name | ScopeKind::SelfRef => name_scope.clone(),
                ScopeKind::None | ScopeKind::Wild => Some(Scope::Wild),
            };
            if let Some(s) = scope {
                set.insert(Cap::new(verb, s));
            }
        }
        set
    }
}

// ---------------------------------------------------------------------------
// Verb tables for each role.
//
// Roles compose by listing their own verbs explicitly (rather than
// `OBSERVER_VERBS + extra`) so a reader can answer "what can this role
// do?" by looking at one place.
// ---------------------------------------------------------------------------

const OBSERVER_VERBS: &[Verb] = &[
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
];

const WORKER_VERBS: &[Verb] = &[
    // observer ↓
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
    // worker additions ↓
    Verb::FS_WRITE,
    Verb::DATA_KV_WRITE,
    Verb::DATA_DB_WRITE,
    Verb::DATA_LOG_WRITE,
    Verb::DATA_INBOX_WRITE,
    Verb::MEMORY_WRITE,
    Verb::IPC_PUBLISH,
    Verb::UI_PROMPT,
    Verb::AI_CHAT,
    Verb::AI_EMBED,
];

const CURATOR_VERBS: &[Verb] = &[
    // worker ↓
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
    Verb::FS_WRITE,
    Verb::DATA_KV_WRITE,
    Verb::DATA_DB_WRITE,
    Verb::DATA_LOG_WRITE,
    Verb::DATA_INBOX_WRITE,
    Verb::MEMORY_WRITE,
    Verb::IPC_PUBLISH,
    Verb::UI_PROMPT,
    Verb::AI_CHAT,
    Verb::AI_EMBED,
    // curator additions ↓
    Verb::FS_DELETE,
    Verb::DATA_KV_DELETE,
];

const CONNECTOR_VERBS: &[Verb] = &[
    // observer ↓
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
    // connector additions ↓
    Verb::NET_DIAL,
    Verb::NET_RESOLVE,
    Verb::NET_PROBE,
    Verb::SECRET_READ,
    Verb::AI_CHAT,
    Verb::AI_CHAT_UNTRUSTED,
    Verb::AI_EMBED,
    Verb::AI_VISION_ANALYZE,
    Verb::BROWSER_TABS_READ,
    Verb::BROWSER_NAV,
    Verb::BROWSER_DOM_READ,
];

const AUTOMATOR_VERBS: &[Verb] = &[
    // curator ↓
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
    Verb::FS_WRITE,
    Verb::DATA_KV_WRITE,
    Verb::DATA_DB_WRITE,
    Verb::DATA_LOG_WRITE,
    Verb::DATA_INBOX_WRITE,
    Verb::MEMORY_WRITE,
    Verb::IPC_PUBLISH,
    Verb::UI_PROMPT,
    Verb::FS_DELETE,
    Verb::DATA_KV_DELETE,
    // automator additions ↓
    Verb::FS_EXEC,
    Verb::PROC_SPAWN,
    Verb::NET_DIAL,
    Verb::NET_RESOLVE,
    Verb::NET_PROBE,
    Verb::SECRET_READ,
    Verb::IPC_INVOKE,
    Verb::TIME_CRON,
    Verb::AI_CHAT,
    Verb::AI_CHAT_UNTRUSTED,
    Verb::AI_EMBED,
    Verb::AI_IMAGE_GENERATE,
    Verb::AI_IMAGE_ANALYZE,
    Verb::AI_AUDIO_TTS,
    Verb::AI_AUDIO_STT,
    Verb::AI_VISION_ANALYZE,
    Verb::AI_VIDEO_GENERATE,
    Verb::AI_VIDEO_ANALYZE,
    Verb::BROWSER_TABS_READ,
    Verb::BROWSER_NAV,
    Verb::BROWSER_DOM_READ,
    Verb::BROWSER_DOM_WRITE,
];

const AGENT_HOST_VERBS: &[Verb] = &[
    // automator ↓
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
    Verb::FS_WRITE,
    Verb::DATA_KV_WRITE,
    Verb::DATA_DB_WRITE,
    Verb::DATA_LOG_WRITE,
    Verb::DATA_INBOX_WRITE,
    Verb::MEMORY_WRITE,
    Verb::IPC_PUBLISH,
    Verb::UI_PROMPT,
    Verb::FS_DELETE,
    Verb::DATA_KV_DELETE,
    Verb::FS_EXEC,
    Verb::PROC_SPAWN,
    Verb::NET_DIAL,
    Verb::NET_RESOLVE,
    Verb::NET_PROBE,
    Verb::SECRET_READ,
    Verb::IPC_INVOKE,
    Verb::TIME_CRON,
    Verb::AI_CHAT,
    Verb::AI_CHAT_UNTRUSTED,
    Verb::AI_EMBED,
    Verb::AI_IMAGE_GENERATE,
    Verb::AI_IMAGE_ANALYZE,
    Verb::AI_AUDIO_TTS,
    Verb::AI_AUDIO_STT,
    Verb::AI_VISION_ANALYZE,
    Verb::AI_VIDEO_GENERATE,
    Verb::AI_VIDEO_ANALYZE,
    // agent-host additions ↓
    Verb::AGENT_SPAWN,
    Verb::AGENT_INVOKE,
    Verb::AGENT_DELEGATE,
    Verb::PROC_SIGNAL,
    Verb::BROWSER_TABS_READ,
    Verb::BROWSER_NAV,
    Verb::BROWSER_DOM_READ,
    Verb::BROWSER_DOM_WRITE,
];

const ADMIN_VERBS: &[Verb] = &[
    // agent-host ↓
    Verb::FS_READ,
    Verb::FS_META,
    Verb::FS_WATCH,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::DATA_KV_READ,
    Verb::DATA_DB_READ,
    Verb::DATA_LOG_READ,
    Verb::DATA_INBOX_READ,
    Verb::MEMORY_READ,
    Verb::AGENT_OBSERVE,
    Verb::IPC_SUBSCRIBE,
    Verb::UI_NOTIFY,
    Verb::TIME_DELAY,
    Verb::FS_WRITE,
    Verb::DATA_KV_WRITE,
    Verb::DATA_DB_WRITE,
    Verb::DATA_LOG_WRITE,
    Verb::DATA_INBOX_WRITE,
    Verb::MEMORY_WRITE,
    Verb::IPC_PUBLISH,
    Verb::UI_PROMPT,
    Verb::FS_DELETE,
    Verb::DATA_KV_DELETE,
    Verb::FS_EXEC,
    Verb::PROC_SPAWN,
    Verb::NET_DIAL,
    Verb::NET_RESOLVE,
    Verb::NET_PROBE,
    Verb::SECRET_READ,
    Verb::IPC_INVOKE,
    Verb::TIME_CRON,
    Verb::AI_CHAT,
    Verb::AI_CHAT_UNTRUSTED,
    Verb::AI_EMBED,
    Verb::AI_IMAGE_GENERATE,
    Verb::AI_IMAGE_ANALYZE,
    Verb::AI_AUDIO_TTS,
    Verb::AI_AUDIO_STT,
    Verb::AI_VISION_ANALYZE,
    Verb::AI_VIDEO_GENERATE,
    Verb::AI_VIDEO_ANALYZE,
    Verb::AGENT_SPAWN,
    Verb::AGENT_INVOKE,
    Verb::AGENT_DELEGATE,
    Verb::PROC_SIGNAL,
    // admin additions ↓
    Verb::SECRET_WRITE,
    Verb::SECRET_GRANT,
    Verb::SYS_CRASH,
    Verb::SYS_CONTAINER,
    Verb::SYS_CONFIG,
    Verb::SYS_EVENTS,
    Verb::SYS_IDENTITY,
    Verb::SYS_SECURITY,
    Verb::SYS_STORAGE,
    Verb::SYS_SERVICE,
    Verb::SYS_PACKAGE,
    Verb::SYS_MOUNT,
    Verb::SYS_SNAPSHOT,
    Verb::SYS_TIME,
    Verb::DATA_BACKUP,
    Verb::NET_LISTEN,
    Verb::NET_RAW,
    Verb::NET_MANAGE,
    Verb::NET_FIREWALL,
    Verb::UI_WINDOW,
    Verb::UI_INPUT,
    Verb::CLIPBOARD_READ,
    Verb::CLIPBOARD_WRITE,
    Verb::UI_ACCESSIBILITY,
    Verb::DEVICE_AUDIO,
    Verb::DEVICE_BLUETOOTH,
    Verb::DEVICE_MEDIA_ROUTE,
    Verb::DEVICE_PRINTER,
    Verb::DEVICE_DISPLAY,
    Verb::DEVICE_CAMERA,
    Verb::DEVICE_MICROPHONE,
    Verb::DEVICE_LOCATION,
    Verb::DEVICE_SENSOR,
    Verb::DEVICE_USB,
    Verb::DESKTOP_WINDOW,
    Verb::BROWSER_TABS_READ,
    Verb::BROWSER_NAV,
    Verb::BROWSER_DOM_READ,
    Verb::BROWSER_DOM_WRITE,
    Verb::BROWSER_INPUT_SECRET,
    Verb::BROWSER_EVAL,
];

/// `kernel` is the OS process itself: every verb in `ALL_VERBS`.
const KERNEL_VERBS: &[Verb] = super::verb::ALL_VERBS;

/// All built-in roles in canonical display order. The `Kernel` role
/// is included here for completeness but should be filtered out of
/// user-facing pickers.
pub const ALL_ROLES: &[Role] = &[
    Role::Observer,
    Role::Worker,
    Role::Curator,
    Role::Connector,
    Role::Automator,
    Role::AgentHost,
    Role::Admin,
    Role::Kernel,
];

/// Roles a user is allowed to pick from a role picker (everything
/// except `Kernel`).
pub fn user_selectable() -> impl Iterator<Item = Role> {
    ALL_ROLES.iter().copied().filter(|r| *r != Role::Kernel)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/role.rs"
    ));
}

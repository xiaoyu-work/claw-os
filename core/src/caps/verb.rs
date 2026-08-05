//! Capability verb identifiers.
//!
//! A [`Verb`] is the *what* half of a capability — e.g. `fs.read`,
//! `net.dial`, `secret.grant`. The other half is the [`Scope`](super::scope::Scope)
//! that bounds it. Verbs form a **closed** set defined by the OS: third-party
//! apps and user-supplied agents can only request verbs that already exist
//! here. New verbs require a code change (and matching catalog entry +
//! enforcement hook), so the surface stays auditable.
//!
//! Internally a `Verb` is a `&'static str` so it costs nothing to copy and
//! compares as a pointer-equivalent. All verbs are constructed exclusively
//! from the [`ALL_VERBS`] table in this module; deserializers and CLI
//! parsers route through [`Verb::parse`], which rejects unknown strings.

use std::fmt;

/// A capability verb. Cheap to clone; pattern-equal to its constant.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Verb(&'static str);

impl Verb {
    /// Internal constructor. Public callers must go through
    /// [`Verb::parse`] so we can validate against [`ALL_VERBS`].
    const fn new(s: &'static str) -> Self {
        Self(s)
    }

    /// String form, suitable for logs, serialization, and audit records.
    pub fn as_str(&self) -> &'static str {
        self.0
    }

    /// Look up a verb by its string identifier. Returns `None` if no
    /// such verb is registered. Case-sensitive: verbs are always
    /// lower-case kebab-with-dots (`fs.read`, `data.kv.write`).
    pub fn parse(s: &str) -> Option<Self> {
        ALL_VERBS.iter().copied().find(|v| v.0 == s)
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl serde::Serialize for Verb {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Verb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <&str as serde::Deserialize>::deserialize(d)?;
        Verb::parse(raw).ok_or_else(|| serde::de::Error::custom(format!("unknown verb: {raw}")))
    }
}

// ---------------------------------------------------------------------------
// Verb registry — single source of truth for "what verbs exist".
//
// Each module of the kernel reaches for one of these constants when it
// needs to gate an action. Adding a verb here costs:
//   1. A line in `ALL_VERBS` below.
//   2. A `CapMeta` entry in `catalog.rs` (otherwise it's invisible to UI).
//   3. Optionally, role memberships in `role.rs`.
// ---------------------------------------------------------------------------

impl Verb {
    // -- File system -------------------------------------------------------
    pub const FS_READ: Verb = Verb::new("fs.read");
    pub const FS_WRITE: Verb = Verb::new("fs.write");
    pub const FS_DELETE: Verb = Verb::new("fs.delete");
    pub const FS_EXEC: Verb = Verb::new("fs.exec");
    pub const FS_WATCH: Verb = Verb::new("fs.watch");
    pub const FS_META: Verb = Verb::new("fs.meta");

    // -- Network -----------------------------------------------------------
    pub const NET_DIAL: Verb = Verb::new("net.dial");
    pub const NET_LISTEN: Verb = Verb::new("net.listen");
    pub const NET_RAW: Verb = Verb::new("net.raw");
    pub const NET_RESOLVE: Verb = Verb::new("net.resolve");
    pub const NET_MANAGE: Verb = Verb::new("net.manage");

    // -- Processes ---------------------------------------------------------
    pub const PROC_SPAWN: Verb = Verb::new("proc.spawn");
    pub const PROC_SIGNAL: Verb = Verb::new("proc.signal");
    pub const PROC_OBSERVE: Verb = Verb::new("proc.observe");

    // -- System state -----------------------------------------------------
    pub const SYS_OBSERVE: Verb = Verb::new("sys.observe");
    pub const SYS_CRASH: Verb = Verb::new("sys.crash");
    pub const SYS_STORAGE: Verb = Verb::new("sys.storage");
    pub const SYS_SERVICE: Verb = Verb::new("sys.service");
    pub const SYS_PACKAGE: Verb = Verb::new("sys.package");
    pub const SYS_MOUNT: Verb = Verb::new("sys.mount");
    pub const SYS_SNAPSHOT: Verb = Verb::new("sys.snapshot");
    pub const SYS_TIME: Verb = Verb::new("sys.time");
    pub const SYS_POWER: Verb = Verb::new("sys.power");
    pub const SYS_KERNEL: Verb = Verb::new("sys.kernel");

    // -- Secrets / credentials --------------------------------------------
    pub const SECRET_READ: Verb = Verb::new("secret.read");
    pub const SECRET_WRITE: Verb = Verb::new("secret.write");
    pub const SECRET_GRANT: Verb = Verb::new("secret.grant");

    // -- Agents (the agent-orchestrating-agents axis) ----------------------
    pub const AGENT_SPAWN: Verb = Verb::new("agent.spawn");
    pub const AGENT_INVOKE: Verb = Verb::new("agent.invoke");
    pub const AGENT_OBSERVE: Verb = Verb::new("agent.observe");
    pub const AGENT_DELEGATE: Verb = Verb::new("agent.delegate");

    // -- Built-in data stores ---------------------------------------------
    pub const DATA_KV_READ: Verb = Verb::new("data.kv.read");
    pub const DATA_KV_WRITE: Verb = Verb::new("data.kv.write");
    pub const DATA_KV_DELETE: Verb = Verb::new("data.kv.delete");
    pub const DATA_DB_READ: Verb = Verb::new("data.db.read");
    pub const DATA_DB_WRITE: Verb = Verb::new("data.db.write");
    pub const DATA_LOG_READ: Verb = Verb::new("data.log.read");
    pub const DATA_LOG_WRITE: Verb = Verb::new("data.log.write");
    pub const DATA_INBOX_READ: Verb = Verb::new("data.inbox.read");
    pub const DATA_INBOX_WRITE: Verb = Verb::new("data.inbox.write");

    // -- Agent memory ------------------------------------------------------
    // Apps that hold this verb can push searchable summaries of their own
    // activity into the agent's memory (FTS5 + semantic). Scope is
    // `self:<app_id>` by convention; the bridge constrains every write to
    // the app's own namespace (`app/<source>`) so a granted app cannot
    // pollute other apps' or the agent's session memory. The user
    // inspects and forgets these rows via `cos agent memory`.
    pub const MEMORY_WRITE: Verb = Verb::new("memory.write");
    /// Read entries the app itself wrote. Scope is `self:<app_id>` —
    /// an app cannot peek into another app's namespace. The agent
    /// runtime reads in-process (no bridge) and is unaffected.
    pub const MEMORY_READ: Verb = Verb::new("memory.read");

    // -- IPC / messaging ---------------------------------------------------
    pub const IPC_PUBLISH: Verb = Verb::new("ipc.publish");
    pub const IPC_SUBSCRIBE: Verb = Verb::new("ipc.subscribe");
    pub const IPC_INVOKE: Verb = Verb::new("ipc.invoke");

    // -- User interface ----------------------------------------------------
    pub const UI_NOTIFY: Verb = Verb::new("ui.notify");
    pub const UI_PROMPT: Verb = Verb::new("ui.prompt");
    pub const UI_WINDOW: Verb = Verb::new("ui.window");
    pub const UI_INPUT: Verb = Verb::new("ui.input");

    // -- Clipboard ---------------------------------------------------------
    pub const CLIPBOARD_READ: Verb = Verb::new("clipboard.read");
    pub const CLIPBOARD_WRITE: Verb = Verb::new("clipboard.write");

    // -- Devices -----------------------------------------------------------
    pub const DEVICE_AUDIO: Verb = Verb::new("device.audio");
    pub const DEVICE_MEDIA_ROUTE: Verb = Verb::new("device.media-route");
    pub const DEVICE_CAMERA: Verb = Verb::new("device.camera");
    pub const DEVICE_MICROPHONE: Verb = Verb::new("device.microphone");
    pub const DEVICE_LOCATION: Verb = Verb::new("device.location");
    pub const DEVICE_SENSOR: Verb = Verb::new("device.sensor");
    pub const DEVICE_USB: Verb = Verb::new("device.usb");

    // -- Time / scheduling -------------------------------------------------
    pub const TIME_CRON: Verb = Verb::new("time.cron");
    pub const TIME_DELAY: Verb = Verb::new("time.delay");

    // -- AI (modality-agnostic gateway) -----------------------------------
    // The kernel routes every cloud / on-device model call through
    // `core/src/ai/gate.rs` so budget, safety, and audit apply uniformly.
    // Origin (trusted | external_content | user_input) is carried as a
    // request field, not a verb; `ai.chat.untrusted` is a hardened variant
    // that *requires* origin=external_content.
    pub const AI_CHAT: Verb = Verb::new("ai.chat");
    pub const AI_CHAT_UNTRUSTED: Verb = Verb::new("ai.chat.untrusted");
    pub const AI_EMBED: Verb = Verb::new("ai.embed");
    pub const AI_IMAGE_GENERATE: Verb = Verb::new("ai.image.generate");
    pub const AI_IMAGE_ANALYZE: Verb = Verb::new("ai.image.analyze");
    pub const AI_AUDIO_TTS: Verb = Verb::new("ai.audio.tts");
    pub const AI_AUDIO_STT: Verb = Verb::new("ai.audio.stt");
    pub const AI_VISION_ANALYZE: Verb = Verb::new("ai.vision.analyze");
    pub const AI_VIDEO_GENERATE: Verb = Verb::new("ai.video.generate");
    pub const AI_VIDEO_ANALYZE: Verb = Verb::new("ai.video.analyze");
    /// User-only verb: lets the owner skip a safety / budget gate for a
    /// single call, app, or session. Apps must never be granted this.
    pub const AI_BYPASS: Verb = Verb::new("ai.bypass");

    // -- Desktop apps ------------------------------------------------------
    // `desktop.launch` gates AI-initiated launches of installed GUI apps.
    // Scope is the `.desktop` AppID (e.g. `com.clawos.Files`); the kernel
    // never lets the agent name a binary path or pass a `-e bash` payload
    // — args are restricted to URI/path substitutions in the entry's
    // `Exec=` line. `cos app exec start <binary>` is the power-user path
    // and is gated separately by `proc.spawn`.
    pub const DESKTOP_LAUNCH: Verb = Verb::new("desktop.launch");
    pub const DESKTOP_WINDOW: Verb = Verb::new("desktop.window");

    // -- Attached browser (WebExtension + Native Messaging) ---------------
    // These verbs gate the *user's* GUI browser (the Chromium that ships
    // with the OS, with the user's logged-in profile). Per-tab actions
    // are scoped to the page's host. Headless browser ops (apps/web →
    // cos-browser) use net.dial instead — that's a different surface.
    pub const BROWSER_TABS_READ: Verb = Verb::new("browser.tabs.read");
    pub const BROWSER_NAV: Verb = Verb::new("browser.nav");
    pub const BROWSER_DOM_READ: Verb = Verb::new("browser.dom.read");
    pub const BROWSER_DOM_WRITE: Verb = Verb::new("browser.dom.write");
    /// Fill into fields the content script classified as
    /// password / credit-card / SSN / other secret. Always Critical;
    /// not in any default role; user must grant per-call.
    pub const BROWSER_INPUT_SECRET: Verb = Verb::new("browser.input.secret");
    /// Run arbitrary JS in the page (`page.eval`). Bypasses the
    /// content-script's safety abstractions; admin only.
    pub const BROWSER_EVAL: Verb = Verb::new("browser.eval");
}

/// Every verb the OS recognises. Order is the canonical display order
/// (UI lists, audit columns, etc).
pub const ALL_VERBS: &[Verb] = &[
    Verb::FS_READ,
    Verb::FS_WRITE,
    Verb::FS_DELETE,
    Verb::FS_EXEC,
    Verb::FS_WATCH,
    Verb::FS_META,
    Verb::NET_DIAL,
    Verb::NET_LISTEN,
    Verb::NET_RAW,
    Verb::NET_RESOLVE,
    Verb::NET_MANAGE,
    Verb::PROC_SPAWN,
    Verb::PROC_SIGNAL,
    Verb::PROC_OBSERVE,
    Verb::SYS_OBSERVE,
    Verb::SYS_CRASH,
    Verb::SYS_STORAGE,
    Verb::SYS_SERVICE,
    Verb::SYS_PACKAGE,
    Verb::SYS_MOUNT,
    Verb::SYS_SNAPSHOT,
    Verb::SYS_TIME,
    Verb::SYS_POWER,
    Verb::SYS_KERNEL,
    Verb::SECRET_READ,
    Verb::SECRET_WRITE,
    Verb::SECRET_GRANT,
    Verb::AGENT_SPAWN,
    Verb::AGENT_INVOKE,
    Verb::AGENT_OBSERVE,
    Verb::AGENT_DELEGATE,
    Verb::DATA_KV_READ,
    Verb::DATA_KV_WRITE,
    Verb::DATA_KV_DELETE,
    Verb::DATA_DB_READ,
    Verb::DATA_DB_WRITE,
    Verb::DATA_LOG_READ,
    Verb::DATA_LOG_WRITE,
    Verb::DATA_INBOX_READ,
    Verb::DATA_INBOX_WRITE,
    Verb::MEMORY_WRITE,
    Verb::MEMORY_READ,
    Verb::IPC_PUBLISH,
    Verb::IPC_SUBSCRIBE,
    Verb::IPC_INVOKE,
    Verb::UI_NOTIFY,
    Verb::UI_PROMPT,
    Verb::UI_WINDOW,
    Verb::UI_INPUT,
    Verb::CLIPBOARD_READ,
    Verb::CLIPBOARD_WRITE,
    Verb::DEVICE_AUDIO,
    Verb::DEVICE_MEDIA_ROUTE,
    Verb::DEVICE_CAMERA,
    Verb::DEVICE_MICROPHONE,
    Verb::DEVICE_LOCATION,
    Verb::DEVICE_SENSOR,
    Verb::DEVICE_USB,
    Verb::TIME_CRON,
    Verb::TIME_DELAY,
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
    Verb::AI_BYPASS,
    Verb::DESKTOP_LAUNCH,
    Verb::DESKTOP_WINDOW,
    Verb::BROWSER_TABS_READ,
    Verb::BROWSER_NAV,
    Verb::BROWSER_DOM_READ,
    Verb::BROWSER_DOM_WRITE,
    Verb::BROWSER_INPUT_SECRET,
    Verb::BROWSER_EVAL,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_verb() {
        assert_eq!(Verb::parse("fs.read"), Some(Verb::FS_READ));
        assert_eq!(
            Verb::parse("device.microphone"),
            Some(Verb::DEVICE_MICROPHONE)
        );
    }

    #[test]
    fn parse_unknown_verb_is_none() {
        assert_eq!(Verb::parse("fs.unknown"), None);
        assert_eq!(Verb::parse(""), None);
        assert_eq!(Verb::parse("FS.READ"), None); // case-sensitive
    }

    #[test]
    fn all_verbs_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for v in ALL_VERBS {
            assert!(seen.insert(v.as_str()), "duplicate verb: {}", v.as_str());
        }
    }

    #[test]
    fn all_verbs_round_trip_through_parse() {
        for v in ALL_VERBS {
            assert_eq!(Verb::parse(v.as_str()), Some(*v));
        }
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(Verb::FS_READ.to_string(), "fs.read");
    }

    #[test]
    fn serde_round_trip() {
        let v = Verb::NET_DIAL;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"net.dial\"");
        let back: Verb = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn serde_rejects_unknown_verb() {
        let result: Result<Verb, _> = serde_json::from_str("\"fs.totally-not-real\"");
        assert!(result.is_err());
    }
}

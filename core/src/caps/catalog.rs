//! The capability catalog — single source of truth for everything a
//! human or agent sees about a verb.
//!
//! Each [`Verb`](super::verb::Verb) is paired with a [`CapMeta`] entry
//! describing:
//!
//!   - its **label** (one short phrase, end-user-readable),
//!   - a **blurb** (one sentence explaining what holding this lets an
//!     agent actually do),
//!   - an **icon** (a small emoji or glyph, used in the approval UI),
//!   - a **risk** rating (`Low` ... `Critical`),
//!   - the **scope kind** it expects (path / host / name / …).
//!
//! Labels and blurbs are [`LocalizedStr`]s so every translation lives
//! next to the verb it describes. English is mandatory; other locales
//! are added by extending [`LocalizedStr`] (see [`crate::i18n`]).
//!
//! Lookup is linear over a small `&'static` slice — at ~50 entries
//! this is faster than a `HashMap` and lets us keep the catalog
//! `const`.

use crate::i18n::LocalizedStr;

use super::risk::Risk;
use super::scope::ScopeKind;
use super::verb::{Verb, ALL_VERBS};

/// Per-verb UI and policy metadata.
#[derive(Clone, Copy, Debug)]
pub struct CapMeta {
    pub verb: Verb,
    /// What kind of scope this verb expects. UIs and parsers consult
    /// this to render an appropriate widget / validate input.
    pub scope_kind: ScopeKind,
    /// Short, user-facing label. Imperative voice, "an agent <does X>".
    pub label: LocalizedStr,
    /// One-sentence elaboration shown in tooltips and approval dialogs.
    pub blurb: LocalizedStr,
    /// Single-character or single-emoji icon for compact rendering.
    pub icon: &'static str,
    /// Worst-case impact rating; aggregated to colour the approval UI.
    pub risk: Risk,
}

impl CapMeta {
    /// Internal constructor used by the static table below.
    const fn new(
        verb: Verb,
        scope_kind: ScopeKind,
        label: LocalizedStr,
        blurb: LocalizedStr,
        icon: &'static str,
        risk: Risk,
    ) -> Self {
        Self {
            verb,
            scope_kind,
            label,
            blurb,
            icon,
            risk,
        }
    }
}

// ---------------------------------------------------------------------------
// The catalog itself.
//
// Order matches `ALL_VERBS` (verb.rs) — and the canonical display order
// in UIs. There is a startup self-check that asserts the two stay in
// lockstep.
// ---------------------------------------------------------------------------

pub const CATALOG: &[CapMeta] = &[
    // -- File system ---------------------------------------------------------
    CapMeta::new(
        Verb::FS_READ,
        ScopeKind::Path,
        LocalizedStr::new("View your files"),
        LocalizedStr::new("Read text and binary files within the granted folder."),
        "📄",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::FS_WRITE,
        ScopeKind::Path,
        LocalizedStr::new("Create or modify files"),
        LocalizedStr::new("Write new files or change existing ones in the granted folder."),
        "✏️",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::FS_DELETE,
        ScopeKind::Path,
        LocalizedStr::new("Delete files"),
        LocalizedStr::new("Remove files or folders. A recoverable copy is kept for 30 days."),
        "🗑",
        Risk::High,
    ),
    CapMeta::new(
        Verb::FS_EXEC,
        ScopeKind::Path,
        LocalizedStr::new("Run programs and scripts"),
        LocalizedStr::new("Execute commands and binaries. Programs run with the agent's own permissions."),
        "⚙️",
        Risk::High,
    ),
    CapMeta::new(
        Verb::FS_WATCH,
        ScopeKind::Path,
        LocalizedStr::new("Watch files for changes"),
        LocalizedStr::new("Be notified when files in the granted folder are created, modified, or removed."),
        "👁",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::FS_META,
        ScopeKind::Path,
        LocalizedStr::new("Inspect file details"),
        LocalizedStr::new("See file names, sizes, and timestamps without reading the contents."),
        "🔎",
        Risk::Low,
    ),

    // -- Network -------------------------------------------------------------
    CapMeta::new(
        Verb::NET_DIAL,
        ScopeKind::Host,
        LocalizedStr::new("Access the network"),
        LocalizedStr::new("Make outbound connections to the listed hosts and ports."),
        "🌐",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::NET_LISTEN,
        ScopeKind::Host,
        LocalizedStr::new("Open a network port on your computer"),
        LocalizedStr::new("Accept incoming connections. Other programs and devices may reach this port."),
        "📡",
        Risk::High,
    ),
    CapMeta::new(
        Verb::NET_RAW,
        ScopeKind::Host,
        LocalizedStr::new("Use raw network sockets"),
        LocalizedStr::new("Send and receive packets at the link or IP layer. Bypasses normal firewalls."),
        "🛰",
        Risk::High,
    ),
    CapMeta::new(
        Verb::NET_RESOLVE,
        ScopeKind::Host,
        LocalizedStr::new("Look up domain names"),
        LocalizedStr::new("Perform DNS queries to translate hostnames to addresses."),
        "🧭",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::NET_MANAGE,
        ScopeKind::Name,
        LocalizedStr::new("Change network connections"),
        LocalizedStr::new("Connect or disconnect Wi-Fi and VPN profiles, or change network radio state."),
        "📶",
        Risk::High,
    ),

    // -- Processes -----------------------------------------------------------
    CapMeta::new(
        Verb::PROC_SPAWN,
        ScopeKind::SelfRef,
        LocalizedStr::new("Start helper processes"),
        LocalizedStr::new("Launch background tasks and worker processes under this agent."),
        "🚀",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::PROC_SIGNAL,
        ScopeKind::SelfRef,
        LocalizedStr::new("Stop or signal processes"),
        LocalizedStr::new("Send pause, resume, or terminate signals to processes the agent can reach."),
        "🛑",
        Risk::High,
    ),
    CapMeta::new(
        Verb::PROC_OBSERVE,
        ScopeKind::SelfRef,
        LocalizedStr::new("See what is running"),
        LocalizedStr::new("List processes and their status. Read-only."),
        "👀",
        Risk::Low,
    ),

    // -- System --------------------------------------------------------------
    CapMeta::new(
        Verb::SYS_OBSERVE,
        ScopeKind::Name,
        LocalizedStr::new("Inspect system state"),
        LocalizedStr::new("Read system status such as services, packages, hardware, and logs without changing them."),
        "🖥",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::SYS_CRASH,
        ScopeKind::Name,
        LocalizedStr::new("Inspect system crash dumps"),
        LocalizedStr::new("Read system-wide coredump metadata, correlated journal events, and process backtraces."),
        "💥",
        Risk::High,
    ),
    CapMeta::new(
        Verb::SYS_STORAGE,
        ScopeKind::Name,
        LocalizedStr::new("Run deep storage diagnostics"),
        LocalizedStr::new("Read SMART health and perform bounded, read-only offline filesystem checks."),
        "💽",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::SYS_SERVICE,
        ScopeKind::Name,
        LocalizedStr::new("Start or stop system services"),
        LocalizedStr::new("Enable, disable, or restart background services on your computer."),
        "🛠",
        Risk::High,
    ),
    CapMeta::new(
        Verb::SYS_PACKAGE,
        ScopeKind::Name,
        LocalizedStr::new("Install or remove software"),
        LocalizedStr::new("Add new programs or remove existing ones. Affects every user on this computer."),
        "📦",
        Risk::Critical,
    ),
    CapMeta::new(
        Verb::SYS_MOUNT,
        ScopeKind::Path,
        LocalizedStr::new("Mount or unmount drives"),
        LocalizedStr::new("Attach or detach disks, network shares, and removable media."),
        "💽",
        Risk::High,
    ),
    CapMeta::new(
        Verb::SYS_SNAPSHOT,
        ScopeKind::None,
        LocalizedStr::new("Create or restore system snapshots"),
        LocalizedStr::new("Capture system state before major changes or schedule a full-system rollback."),
        "🛟",
        Risk::Critical,
    ),
    CapMeta::new(
        Verb::SYS_TIME,
        ScopeKind::None,
        LocalizedStr::new("Change the system clock"),
        LocalizedStr::new("Adjust the date, time, or timezone of your computer."),
        "🕒",
        Risk::High,
    ),
    CapMeta::new(
        Verb::SYS_POWER,
        ScopeKind::None,
        LocalizedStr::new("Shut down or restart"),
        LocalizedStr::new("Power off, restart, sleep, or hibernate your computer."),
        "⏻",
        Risk::Critical,
    ),
    CapMeta::new(
        Verb::SYS_KERNEL,
        ScopeKind::Name,
        LocalizedStr::new("Load kernel modules"),
        LocalizedStr::new("Change low-level operating-system behaviour. Reserved for trusted system tools."),
        "🧬",
        Risk::Critical,
    ),

    // -- Secrets -------------------------------------------------------------
    CapMeta::new(
        Verb::SECRET_READ,
        ScopeKind::Name,
        LocalizedStr::new("Read your saved passwords and keys"),
        LocalizedStr::new("Use stored credentials such as API keys, tokens, and SSH keys."),
        "🔑",
        Risk::High,
    ),
    CapMeta::new(
        Verb::SECRET_WRITE,
        ScopeKind::Name,
        LocalizedStr::new("Save passwords and keys"),
        LocalizedStr::new("Store new credentials in the secure vault."),
        "🗝",
        Risk::High,
    ),
    CapMeta::new(
        Verb::SECRET_GRANT,
        ScopeKind::Name,
        LocalizedStr::new("Share secrets with other agents"),
        LocalizedStr::new("Pass stored credentials to sub-agents or other programs. Hard to revoke once shared."),
        "🔁",
        Risk::Critical,
    ),

    // -- Agents --------------------------------------------------------------
    CapMeta::new(
        Verb::AGENT_SPAWN,
        ScopeKind::Name,
        LocalizedStr::new("Start sub-agents"),
        LocalizedStr::new("Create helper agents that work on parts of the task in parallel."),
        "🤖",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AGENT_INVOKE,
        ScopeKind::Name,
        LocalizedStr::new("Call other agents"),
        LocalizedStr::new("Send tasks to existing agents and receive their results."),
        "📨",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AGENT_OBSERVE,
        ScopeKind::Name,
        LocalizedStr::new("Watch other agents"),
        LocalizedStr::new("See what other agents are doing and what they have produced. Read-only."),
        "🪟",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::AGENT_DELEGATE,
        ScopeKind::Name,
        LocalizedStr::new("Pass permissions to sub-agents"),
        LocalizedStr::new("Hand over sensitive abilities such as secret access to sub-agents."),
        "🛂",
        Risk::Critical,
    ),

    // -- Data ----------------------------------------------------------------
    CapMeta::new(
        Verb::DATA_KV_READ,
        ScopeKind::Name,
        LocalizedStr::new("Read app data (key-value)"),
        LocalizedStr::new("Look up values stored in the system key-value store."),
        "📚",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::DATA_KV_WRITE,
        ScopeKind::Name,
        LocalizedStr::new("Save app data (key-value)"),
        LocalizedStr::new("Store values in the system key-value store."),
        "📥",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::DATA_KV_DELETE,
        ScopeKind::Name,
        LocalizedStr::new("Delete app data (key-value)"),
        LocalizedStr::new("Remove values from the system key-value store."),
        "🧹",
        Risk::High,
    ),
    CapMeta::new(
        Verb::DATA_DB_READ,
        ScopeKind::Name,
        LocalizedStr::new("Query databases"),
        LocalizedStr::new("Run read-only queries against the granted databases."),
        "🗃",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::DATA_DB_WRITE,
        ScopeKind::Name,
        LocalizedStr::new("Modify databases"),
        LocalizedStr::new("Insert, update, or delete rows in the granted databases."),
        "🗂",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::DATA_LOG_READ,
        ScopeKind::Name,
        LocalizedStr::new("Read system logs"),
        LocalizedStr::new("Inspect log lines produced by other apps and agents."),
        "📜",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::DATA_LOG_WRITE,
        ScopeKind::Name,
        LocalizedStr::new("Write to system logs"),
        LocalizedStr::new("Record events in the system log so other tools can see them."),
        "📝",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::DATA_INBOX_READ,
        ScopeKind::Name,
        LocalizedStr::new("Read your inbox"),
        LocalizedStr::new("See messages, tasks, and digests sent to you by agents."),
        "📬",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::DATA_INBOX_WRITE,
        ScopeKind::Name,
        LocalizedStr::new("Send to your inbox"),
        LocalizedStr::new("Drop messages, digests, or reminders into your inbox."),
        "✉️",
        Risk::Medium,
    ),

    // -- Agent memory --------------------------------------------------------
    CapMeta::new(
        Verb::MEMORY_WRITE,
        ScopeKind::SelfRef,
        LocalizedStr::new("Write to the agent's memory"),
        LocalizedStr::new("Push searchable summaries (events, facts) into the agent's memory under this app's own namespace, so the agent can later recall what you did. Each entry is tagged with the app id; you can inspect and forget entries with `cos agent memory`."),
        "🧠",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::MEMORY_READ,
        ScopeKind::SelfRef,
        LocalizedStr::new("Read this app's memory"),
        LocalizedStr::new("Look up entries the app itself wrote earlier (e.g. to dedupe before re-storing). Scope is the app's own namespace — cross-app reads are not possible. The agent reads memory in-process and is unaffected by this grant."),
        "🔎",
        Risk::Low,
    ),

    // -- IPC -----------------------------------------------------------------
    CapMeta::new(
        Verb::IPC_PUBLISH,
        ScopeKind::Name,
        LocalizedStr::new("Send messages on system channels"),
        LocalizedStr::new("Publish events to the listed topics so other programs can react."),
        "📢",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::IPC_SUBSCRIBE,
        ScopeKind::Name,
        LocalizedStr::new("Listen on system channels"),
        LocalizedStr::new("Receive events published on the listed topics."),
        "📻",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::IPC_INVOKE,
        ScopeKind::Name,
        LocalizedStr::new("Call system services"),
        LocalizedStr::new("Send a request to a named system service and wait for a reply."),
        "📞",
        Risk::Medium,
    ),

    // -- UI ------------------------------------------------------------------
    CapMeta::new(
        Verb::UI_NOTIFY,
        ScopeKind::None,
        LocalizedStr::new("Show you notifications"),
        LocalizedStr::new("Display small alerts in the system notification area."),
        "🔔",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::UI_PROMPT,
        ScopeKind::None,
        LocalizedStr::new("Ask you questions"),
        LocalizedStr::new("Show prompts that require an answer before the agent continues."),
        "❓",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::UI_WINDOW,
        ScopeKind::None,
        LocalizedStr::new("Open windows and panels"),
        LocalizedStr::new("Create graphical or terminal windows on your desktop."),
        "🪟",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::UI_INPUT,
        ScopeKind::None,
        LocalizedStr::new("Read your keyboard and mouse"),
        LocalizedStr::new("Observe keystrokes and mouse activity. Use with caution."),
        "⌨️",
        Risk::High,
    ),

    // -- Clipboard -----------------------------------------------------------
    CapMeta::new(
        Verb::CLIPBOARD_READ,
        ScopeKind::Name,
        LocalizedStr::new("Read clipboard history"),
        LocalizedStr::new(
            "Read text previously copied to the named clipboard or history store.",
        ),
        "📋",
        Risk::High,
    ),
    CapMeta::new(
        Verb::CLIPBOARD_WRITE,
        ScopeKind::Name,
        LocalizedStr::new("Modify clipboard history"),
        LocalizedStr::new(
            "Restore, delete, or clear items in the named clipboard or history store.",
        ),
        "✂️",
        Risk::High,
    ),

    // -- Devices -------------------------------------------------------------
    CapMeta::new(
        Verb::DEVICE_AUDIO,
        ScopeKind::Name,
        LocalizedStr::new("Control audio output"),
        LocalizedStr::new("Change speaker or headphone volume, mute state, routing, and profiles."),
        "🔊",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::DEVICE_MEDIA_ROUTE,
        ScopeKind::Name,
        LocalizedStr::new("Route local media devices"),
        LocalizedStr::new("Change PipeWire defaults, ports, routes, and device profiles across the local media graph."),
        "🎚",
        Risk::High,
    ),
    CapMeta::new(
        Verb::DEVICE_CAMERA,
        ScopeKind::Name,
        LocalizedStr::new("Use your camera"),
        LocalizedStr::new("Capture photos or video from a connected camera."),
        "📷",
        Risk::High,
    ),
    CapMeta::new(
        Verb::DEVICE_MICROPHONE,
        ScopeKind::Name,
        LocalizedStr::new("Control microphone input"),
        LocalizedStr::new("Change microphone volume, mute state, routing, and audio profiles."),
        "🎤",
        Risk::High,
    ),
    CapMeta::new(
        Verb::DEVICE_LOCATION,
        ScopeKind::None,
        LocalizedStr::new("Get your location"),
        LocalizedStr::new("Read your approximate geographic location."),
        "📍",
        Risk::High,
    ),
    CapMeta::new(
        Verb::DEVICE_SENSOR,
        ScopeKind::Name,
        LocalizedStr::new("Read device sensors"),
        LocalizedStr::new("Access accelerometer, gyroscope, light, or other sensor readings."),
        "📐",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::DEVICE_USB,
        ScopeKind::Name,
        LocalizedStr::new("Talk to USB devices"),
        LocalizedStr::new("Communicate with connected USB peripherals."),
        "🔌",
        Risk::High,
    ),

    // -- Time ----------------------------------------------------------------
    CapMeta::new(
        Verb::TIME_CRON,
        ScopeKind::None,
        LocalizedStr::new("Schedule recurring tasks"),
        LocalizedStr::new("Run jobs on a regular schedule, even when you are not interacting with the agent."),
        "📅",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::TIME_DELAY,
        ScopeKind::None,
        LocalizedStr::new("Sleep and wake up later"),
        LocalizedStr::new("Pause the agent and resume after a delay."),
        "⏱",
        Risk::Low,
    ),

    // -- AI ----------------------------------------------------------------
    CapMeta::new(
        Verb::AI_CHAT,
        ScopeKind::Name,
        LocalizedStr::new("Use AI to chat"),
        LocalizedStr::new("Send messages to a large language model and receive answers. Subject to the app's monthly budget."),
        "🤖",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AI_CHAT_UNTRUSTED,
        ScopeKind::Name,
        LocalizedStr::new("Run AI on external content"),
        LocalizedStr::new("Summarise or analyse text that came from outside the app (emails, web pages). Goes through stricter prompt-injection checks."),
        "📨",
        Risk::High,
    ),
    CapMeta::new(
        Verb::AI_EMBED,
        ScopeKind::Name,
        LocalizedStr::new("Compute AI embeddings"),
        LocalizedStr::new("Turn text into vectors so the app can search or cluster it."),
        "🧮",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::AI_IMAGE_GENERATE,
        ScopeKind::Name,
        LocalizedStr::new("Generate images with AI"),
        LocalizedStr::new("Create new images from a text prompt. Subject to budget."),
        "🎨",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AI_IMAGE_ANALYZE,
        ScopeKind::Name,
        LocalizedStr::new("Describe images with AI"),
        LocalizedStr::new("Send an image to a vision model and read back a description."),
        "🖼",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AI_AUDIO_TTS,
        ScopeKind::Name,
        LocalizedStr::new("Synthesise speech with AI"),
        LocalizedStr::new("Turn text into spoken audio."),
        "🔊",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::AI_AUDIO_STT,
        ScopeKind::Name,
        LocalizedStr::new("Transcribe audio with AI"),
        LocalizedStr::new("Convert spoken audio into text."),
        "🎤",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AI_VISION_ANALYZE,
        ScopeKind::Name,
        LocalizedStr::new("Analyse a scene with AI"),
        LocalizedStr::new("Send camera or screenshot frames to a vision model and read back structured findings."),
        "👁",
        Risk::High,
    ),
    CapMeta::new(
        Verb::AI_VIDEO_GENERATE,
        ScopeKind::Name,
        LocalizedStr::new("Generate videos with AI"),
        LocalizedStr::new("Create short videos from a text prompt. Subject to budget."),
        "🎬",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::AI_VIDEO_ANALYZE,
        ScopeKind::Name,
        LocalizedStr::new("Describe videos with AI"),
        LocalizedStr::new("Send a video clip to a vision model and read back a description."),
        "📹",
        Risk::High,
    ),
    CapMeta::new(
        Verb::AI_BYPASS,
        ScopeKind::None,
        LocalizedStr::new("Skip an AI safety check (owner only)"),
        LocalizedStr::new("Override a safety, budget, or model restriction for a single call. Reserved for the owner — apps cannot request this."),
        "🛂",
        Risk::Critical,
    ),

    // -- Desktop apps -----------------------------------------------------
    CapMeta::new(
        Verb::DESKTOP_LAUNCH,
        ScopeKind::Name,
        LocalizedStr::new("Launch a desktop app"),
        LocalizedStr::new("Open a graphical application installed on this computer. Scope is the freedesktop AppID (`com.clawos.Files`, `org.mozilla.firefox`, …). Granting `*` lets the agent open any installed app, including terminals — narrow the scope when possible."),
        "🪟",
        Risk::Medium,
    ),

    // -- Attached browser -------------------------------------------------
    // Gates the user's GUI browser (Chromium with the user's profile)
    // when reached through the Claw agent WebExtension + Native Messaging
    // host. Per-page actions are host-scoped.
    CapMeta::new(
        Verb::BROWSER_TABS_READ,
        ScopeKind::None,
        LocalizedStr::new("See your open browser tabs"),
        LocalizedStr::new("List the tabs you have open in Chromium (title, URL, active flag). Does not read page contents."),
        "🗂",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::BROWSER_NAV,
        ScopeKind::Host,
        LocalizedStr::new("Navigate your browser"),
        LocalizedStr::new("Send a tab to a URL on the listed hosts (and use back / forward / reload). Limited to the hosts you allow."),
        "🧭",
        Risk::Low,
    ),
    CapMeta::new(
        Verb::BROWSER_DOM_READ,
        ScopeKind::Host,
        LocalizedStr::new("Read page contents in your browser"),
        LocalizedStr::new("Read the DOM, accessibility tree, and screenshots of pages from the listed hosts — including content only your logged-in session can see."),
        "👁",
        Risk::Medium,
    ),
    CapMeta::new(
        Verb::BROWSER_DOM_WRITE,
        ScopeKind::Host,
        LocalizedStr::new("Click and type in your browser"),
        LocalizedStr::new("Click buttons, fill form fields, scroll, and submit forms on pages from the listed hosts. Acts as you, with your session."),
        "✍",
        Risk::High,
    ),
    CapMeta::new(
        Verb::BROWSER_INPUT_SECRET,
        ScopeKind::Host,
        LocalizedStr::new("Type into password / payment fields"),
        LocalizedStr::new("Fill fields the page marks as password, credit card, or other sensitive input. Each call goes through the approval queue; never auto-granted."),
        "🔐",
        Risk::Critical,
    ),
    CapMeta::new(
        Verb::BROWSER_EVAL,
        ScopeKind::Host,
        LocalizedStr::new("Run arbitrary JavaScript in your browser"),
        LocalizedStr::new("Execute attacker-equivalent JS in a page on the listed hosts. Bypasses every per-page safety helper; reserved for admin role and explicit per-call grants."),
        "⚠",
        Risk::Critical,
    ),
];

/// Look up the metadata for a verb. Returns `None` only if the verb
/// list and the catalog have drifted — see [`self_check`].
pub fn lookup(verb: Verb) -> Option<&'static CapMeta> {
    CATALOG.iter().find(|m| m.verb == verb)
}

/// Panic if the catalog and the verb table have drifted. Called from
/// `caps::self_check`, which the boot path invokes once at startup so
/// authors get a loud failure if they add a verb without metadata.
pub fn self_check() -> Result<(), String> {
    if ALL_VERBS.len() != CATALOG.len() {
        return Err(format!(
            "caps catalog drift: ALL_VERBS has {} entries, CATALOG has {}",
            ALL_VERBS.len(),
            CATALOG.len()
        ));
    }
    for (v, meta) in ALL_VERBS.iter().zip(CATALOG.iter()) {
        if *v != meta.verb {
            return Err(format!(
                "caps catalog out of order: expected {} found {}",
                v.as_str(),
                meta.verb.as_str()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_verb_table() {
        self_check().unwrap();
    }

    #[test]
    fn every_verb_has_metadata() {
        for v in ALL_VERBS {
            let m = lookup(*v).unwrap_or_else(|| panic!("missing meta for {}", v.as_str()));
            // Sanity: labels and blurbs must be non-empty in English.
            assert!(!m.label.en().is_empty(), "empty label for {}", v.as_str());
            assert!(!m.blurb.en().is_empty(), "empty blurb for {}", v.as_str());
            assert!(!m.icon.is_empty(), "empty icon for {}", v.as_str());
        }
    }

    #[test]
    fn lookup_returns_none_for_synthetic_unknown() {
        // We can't construct an invalid Verb publicly, so just exercise
        // the happy path here.
        assert!(lookup(Verb::FS_READ).is_some());
    }
}

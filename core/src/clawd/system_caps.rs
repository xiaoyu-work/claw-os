//! Baseline authority of the first-party system Agent.
//!
//! A system Agent runs inside root `clawd` on behalf of a user who is
//! usually not root. Its default capability set is therefore the blast
//! radius of every prompt injection, hostile document and compromised
//! MCP server the Agent ever reads, and it must not be derived from
//! anything the Agent — or the process that asked for it — can
//! influence.
//!
//! ## What the baseline is
//!
//! [`BASELINE`] records one explicit decision per catalog verb. It is
//! *not* a function of [`Risk`]: a Low-risk verb that addresses an
//! arbitrary host (`browser.nav`) or an arbitrary path (`fs.read`) is
//! a bigger lever than a Medium-risk verb that addresses nothing.
//! Every verb whose resource can name somebody else's data, another
//! machine, a new process, a credential, or machine-wide state is
//! [`Baseline::Denied`] and has to arrive through an authenticated
//! task/session delegation or an exact, one-shot user approval.
//!
//! The remainder is the smallest set an ordinary owner-scoped
//! conversation needs: reading and writing the owner's own files, its
//! own memory and process-registry rows, read-only system observation,
//! the owner-partitioned App data stores, the model itself, and the
//! handful of verbs that carry no resource at all.
//!
//! ## What the baseline is not
//!
//! * It is never `Scope::Wild` for a resource-addressing verb. `Wild`
//!   covers *every* scope of *every* kind, so handing it to a
//!   Path/Host/Name verb is indistinguishable from `fs.read:/**`.
//! * It is never widened by euid, role name, prompt text, model
//!   output, controlling terminal or socket group. The owner override
//!   moves *paths*, not authority, and uid 0 gets the same table as
//!   everybody else.
//! * It is never the place an approval lands. Approved grants are
//!   consumed once, at the gate, by [`crate::caps::require`]; nothing
//!   here is rewritten when a user says yes.

use crate::caps::{Cap, CapSet, Risk, Scope, ScopeKind, Verb};
use crate::session::SessionOrigin;
use std::path::{Path, PathBuf};

/// The one decision the daemon makes about a verb before any
/// delegation or approval is involved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Baseline {
    /// Requires an authenticated task/session delegation or an exact
    /// approved grant. This is the default for anything absent from
    /// [`BASELINE`], so a newly added verb fails closed.
    Denied,
    /// Path verbs, bounded to the owner's own roots — the canonical
    /// passwd home plus the daemon-side per-user Agent state
    /// directory, which lives outside it.
    OwnerPaths,
    /// Name verbs bounded to an explicit list of resource names.
    Names(&'static [&'static str]),
    /// Name verbs whose resource is either owner-partitioned (the App
    /// key-value / database / inbox / log stores, all resolved under
    /// the owner's `HOME` for a routed job) or daemon-derived (the
    /// configured model, an installed App id), and read-only or
    /// separately attenuated at the point of use.
    AnyName,
    /// `SelfRef` verbs that address only the session's own rows. There
    /// is no narrower representable wildcard for a self-reference, and
    /// the stores behind them (memory database, process registry) are
    /// already partitioned per owner.
    SelfScoped,
    /// The catalog declares this verb carries no resource at all.
    Resourceless,
}

/// Read-only observation domains an owner-facing Agent gets by default.
///
/// Each one answers a question about the device sitting in front of the
/// owner — is the battery low, is the microphone muted, which monitor
/// is primary, how full is the disk — and none of them names another
/// principal. `sys.observe:**` used to cover far more than that, so the
/// list is exhaustive and everything outside it needs an exact
/// approval:
///
/// * `desktop` lists the windows and shell state of whoever holds the
///   session, which is other people's activity on a multi-seat host;
/// * `identities` is the account database itself;
/// * a systemd unit name reaches `user@<uid>.service` and any other
///   account's units;
/// * `firewall` and `system-snapshots` describe the machine's security
///   posture and administrative state rather than owner-facing device
///   status;
/// * a package name is a different namespace borrowing this verb.
const OBSERVABLE_DEVICE_DOMAINS: &[&str] = &[
    "accessibility",
    "audio",
    "bluetooth",
    "camera",
    "display",
    "hardware",
    "network",
    "power",
    "printing",
    "storage",
    "usb",
];

/// Explicit baseline decision for every verb in the catalog.
///
/// Kept in catalog order. A unit test asserts this table and
/// [`crate::caps::catalog::CATALOG`] describe exactly the same verbs,
/// so a new verb cannot be added without deciding what an ambient
/// system Agent may do with it.
const BASELINE: &[(Verb, Baseline)] = &[
    // -- File system -------------------------------------------------
    // Reads and writes stay inside the owner's roots. The `/**` this
    // replaces gave a task owned by an unprivileged user the root
    // daemon's view of every home, `/etc/shadow` and `/proc`.
    (Verb::FS_READ, Baseline::OwnerPaths),
    (Verb::FS_WRITE, Baseline::OwnerPaths),
    (Verb::FS_DELETE, Baseline::Denied),
    // Executing a program is the whole escalation, whatever the path.
    (Verb::FS_EXEC, Baseline::Denied),
    (Verb::FS_WATCH, Baseline::OwnerPaths),
    (Verb::FS_META, Baseline::OwnerPaths),
    // -- Network -----------------------------------------------------
    // Any host means link-local metadata services, the LAN, and every
    // exfiltration endpoint. Hosts come from a delegation or approval.
    (Verb::NET_DIAL, Baseline::Denied),
    (Verb::NET_LISTEN, Baseline::Denied),
    (Verb::NET_RAW, Baseline::Denied),
    // Resolution alone is a working exfiltration and discovery channel.
    (Verb::NET_RESOLVE, Baseline::Denied),
    // A probe reaches the named endpoint, even though no socket reaches the App.
    (Verb::NET_PROBE, Baseline::Denied),
    (Verb::NET_MANAGE, Baseline::Denied),
    (Verb::NET_FIREWALL, Baseline::Denied),
    // -- Processes ---------------------------------------------------
    (Verb::PROC_SPAWN, Baseline::Denied),
    (Verb::PROC_SIGNAL, Baseline::Denied),
    (Verb::PROC_OBSERVE, Baseline::SelfScoped),
    // -- System state ------------------------------------------------
    // Read-only status of the owner's own device. Every mutating
    // sibling below is denied, and so is every observation domain that
    // describes another principal (see `OBSERVABLE_DEVICE_DOMAINS`).
    (
        Verb::SYS_OBSERVE,
        Baseline::Names(OBSERVABLE_DEVICE_DOMAINS),
    ),
    (Verb::SYS_CRASH, Baseline::Denied),
    (Verb::SYS_CONTAINER, Baseline::Denied),
    (Verb::SYS_CONFIG, Baseline::Denied),
    (Verb::SYS_EVENTS, Baseline::Denied),
    (Verb::SYS_IDENTITY, Baseline::Denied),
    (Verb::SYS_SECURITY, Baseline::Denied),
    (Verb::SYS_STORAGE, Baseline::Denied),
    (Verb::SYS_SERVICE, Baseline::Denied),
    (Verb::SYS_PACKAGE, Baseline::Denied),
    (Verb::SYS_MOUNT, Baseline::Denied),
    (Verb::SYS_SNAPSHOT, Baseline::Denied),
    (Verb::SYS_TIME, Baseline::Denied),
    (Verb::SYS_POWER, Baseline::Denied),
    (Verb::SYS_KERNEL, Baseline::Denied),
    // -- Secrets -----------------------------------------------------
    (Verb::SECRET_READ, Baseline::Denied),
    (Verb::SECRET_WRITE, Baseline::Denied),
    (Verb::SECRET_GRANT, Baseline::Denied),
    // -- Agents ------------------------------------------------------
    // Spawning or delegating hands authority to something the user
    // never saw; invoking an installed App does not, because the App's
    // capabilities are derived by intersecting this very set.
    (Verb::AGENT_SPAWN, Baseline::Denied),
    (Verb::AGENT_INVOKE, Baseline::AnyName),
    (Verb::AGENT_OBSERVE, Baseline::AnyName),
    (Verb::AGENT_DELEGATE, Baseline::Denied),
    // -- Built-in data stores ----------------------------------------
    // Owner-partitioned: a routed job resolves them under the owner's
    // own home.
    (Verb::DATA_KV_READ, Baseline::AnyName),
    (Verb::DATA_KV_WRITE, Baseline::AnyName),
    (Verb::DATA_KV_DELETE, Baseline::Denied),
    (Verb::DATA_DB_READ, Baseline::AnyName),
    (Verb::DATA_DB_WRITE, Baseline::AnyName),
    (Verb::DATA_LOG_READ, Baseline::AnyName),
    (Verb::DATA_LOG_WRITE, Baseline::AnyName),
    (Verb::DATA_INBOX_READ, Baseline::AnyName),
    (Verb::DATA_INBOX_WRITE, Baseline::AnyName),
    (Verb::DATA_BACKUP, Baseline::Denied),
    // -- Memory ------------------------------------------------------
    (Verb::MEMORY_WRITE, Baseline::SelfScoped),
    (Verb::MEMORY_READ, Baseline::SelfScoped),
    // -- IPC ---------------------------------------------------------
    // Queues, locks and pipes live in the shared daemon data directory
    // and are addressed by another session's id, so they are a
    // cross-user channel rather than owner-scoped state.
    (Verb::IPC_PUBLISH, Baseline::Denied),
    (Verb::IPC_SUBSCRIBE, Baseline::Denied),
    (Verb::IPC_INVOKE, Baseline::Denied),
    // -- UI ----------------------------------------------------------
    (Verb::UI_NOTIFY, Baseline::Resourceless),
    // Asking the owner a question is the mechanism consent runs on.
    (Verb::UI_PROMPT, Baseline::Resourceless),
    (Verb::UI_WINDOW, Baseline::Denied),
    (Verb::UI_INPUT, Baseline::Denied),
    (Verb::CLIPBOARD_READ, Baseline::Denied),
    (Verb::CLIPBOARD_WRITE, Baseline::Denied),
    (Verb::UI_ACCESSIBILITY, Baseline::Denied),
    // -- Devices -----------------------------------------------------
    (Verb::DEVICE_AUDIO, Baseline::Denied),
    (Verb::DEVICE_BLUETOOTH, Baseline::Denied),
    (Verb::DEVICE_MEDIA_ROUTE, Baseline::Denied),
    (Verb::DEVICE_PRINTER, Baseline::Denied),
    (Verb::DEVICE_DISPLAY, Baseline::Denied),
    (Verb::DEVICE_CAMERA, Baseline::Denied),
    (Verb::DEVICE_MICROPHONE, Baseline::Denied),
    (Verb::DEVICE_LOCATION, Baseline::Denied),
    (Verb::DEVICE_SENSOR, Baseline::Denied),
    (Verb::DEVICE_USB, Baseline::Denied),
    // -- Time --------------------------------------------------------
    // A cron entry is authority that outlives the conversation.
    (Verb::TIME_CRON, Baseline::Denied),
    (Verb::TIME_DELAY, Baseline::Resourceless),
    // -- AI ----------------------------------------------------------
    // The model name is daemon configuration, and the provider, budget
    // and audit trail are owned by the system, so ordinary conversation
    // and owner-scoped media work stay available.
    (Verb::AI_CHAT, Baseline::AnyName),
    (Verb::AI_CHAT_UNTRUSTED, Baseline::Denied),
    (Verb::AI_EMBED, Baseline::AnyName),
    (Verb::AI_IMAGE_GENERATE, Baseline::AnyName),
    (Verb::AI_IMAGE_ANALYZE, Baseline::AnyName),
    (Verb::AI_AUDIO_TTS, Baseline::AnyName),
    (Verb::AI_AUDIO_STT, Baseline::AnyName),
    (Verb::AI_VISION_ANALYZE, Baseline::Denied),
    (Verb::AI_VIDEO_GENERATE, Baseline::AnyName),
    (Verb::AI_VIDEO_ANALYZE, Baseline::Denied),
    (Verb::AI_BYPASS, Baseline::Denied),
    // -- Desktop -----------------------------------------------------
    // Launching a desktop entry runs an arbitrary program.
    (Verb::DESKTOP_LAUNCH, Baseline::Denied),
    (Verb::DESKTOP_WINDOW, Baseline::Denied),
    // -- Browser -----------------------------------------------------
    (Verb::BROWSER_TABS_READ, Baseline::Resourceless),
    // Navigation and DOM access are host-addressed fetches.
    (Verb::BROWSER_NAV, Baseline::Denied),
    (Verb::BROWSER_DOM_READ, Baseline::Denied),
    (Verb::BROWSER_DOM_WRITE, Baseline::Denied),
    (Verb::BROWSER_INPUT_SECRET, Baseline::Denied),
    (Verb::BROWSER_EVAL, Baseline::Denied),
];

/// Baseline decision for one verb. Anything the table does not name is
/// denied, so adding a verb to the catalog without deciding its
/// baseline cannot silently widen an ambient Agent.
fn baseline_for(verb: Verb) -> Baseline {
    BASELINE
        .iter()
        .find(|(candidate, _)| *candidate == verb)
        .map_or(Baseline::Denied, |(_, baseline)| *baseline)
}

/// Path roots a system Agent owned by `owner_uid` may address.
///
/// The home is the canonical passwd home the daemon resolved for that
/// uid; the second root is the daemon-side per-user Agent state
/// directory (memory database, notes, semantic index), which lives
/// under the daemon data directory rather than inside the home.
fn owner_path_roots(owner_uid: u32, owner_home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![owner_home.to_path_buf()];
    let state = crate::paths::clawd_user_agent_state_dir(owner_uid);
    if !state.starts_with(owner_home) {
        roots.push(state);
    }
    roots
}

/// Scopes one verb receives at baseline. An empty result means denied.
///
/// The catalog's declared [`ScopeKind`] and the baseline decision must
/// agree; a mismatch (a Path verb marked [`Baseline::AnyName`], a Name
/// verb marked [`Baseline::Resourceless`], …) yields no scopes at all
/// rather than an untyped wildcard.
fn baseline_scopes(verb: Verb, scope_kind: ScopeKind, roots: &[PathBuf]) -> Vec<Scope> {
    match (baseline_for(verb), scope_kind) {
        (Baseline::OwnerPaths, ScopeKind::Path) => roots
            .iter()
            .map(|root| Scope::path(format!("{}/**", root.display())))
            .collect(),
        (Baseline::Names(names), ScopeKind::Name) => {
            names.iter().map(|name| Scope::name(*name)).collect()
        }
        (Baseline::AnyName, ScopeKind::Name) => vec![Scope::name("**")],
        (Baseline::SelfScoped, ScopeKind::SelfRef) => vec![Scope::Wild],
        (Baseline::Resourceless, ScopeKind::None) => vec![Scope::Wild],
        _ => Vec::new(),
    }
}

/// Capabilities a system Agent holds before any authenticated
/// delegation or approved grant.
///
/// `owner_uid` and `owner_home` are daemon-derived facts about the
/// account the Agent acts for — the peer credentials of the socket and
/// the passwd home for that uid. They select *which* files the Agent
/// may touch; they never select how much authority it has.
pub fn system_agent_caps(owner_uid: u32, owner_home: &Path) -> CapSet {
    let roots = owner_path_roots(owner_uid, owner_home);
    let mut caps = CapSet::new();
    for meta in crate::caps::catalog::CATALOG {
        for scope in baseline_scopes(meta.verb, meta.scope_kind, &roots) {
            caps.insert(Cap::new(meta.verb, scope));
        }
    }
    caps
}

/// Resolve the path root a system Agent owned by `owner_uid` is bounded
/// to, from the kernel's passwd view.
///
/// Every baseline and delegation derivation goes through here so they
/// cannot disagree: the home is canonicalized, must exist, and must be
/// owned by that uid. There is no fallback — a home the daemon cannot
/// verify yields no capabilities at all.
pub fn verified_owner_home(owner_uid: u32) -> Result<PathBuf, String> {
    crate::paths::verified_home_for_uid(owner_uid)
}

/// Clamp what a trusted-session override may carry to the policy that
/// matches the session's provenance.
///
/// The stored capability set is authority the daemon wrote earlier;
/// this is the point where it is re-derived rather than trusted, so a
/// session row that acquired something broader — under an older
/// policy, from a tampered session directory, or via a future code
/// path — cannot inject it into a worker. The result is always an
/// intersection or an exact re-admission: never additive, never
/// widened, and never a place an approval becomes standing authority.
pub fn clamp_for_origin(
    stored: &CapSet,
    origin: SessionOrigin,
    owner_uid: u32,
    owner_home: &Path,
) -> CapSet {
    let mut caps = clamp_to_owner_baseline(stored, owner_uid, owner_home);
    let Some(executor) = delegated_executor_verb(origin) else {
        return caps;
    };
    for cap in stored.iter() {
        if delegated_cap_is_admissible(cap, executor) {
            caps.insert(cap.clone());
        }
    }
    caps
}

/// Clamp to the ambient baseline: what an Agent gets with no
/// delegation at all.
pub fn clamp_to_owner_baseline(stored: &CapSet, owner_uid: u32, owner_home: &Path) -> CapSet {
    system_agent_caps(owner_uid, owner_home).intersect(stored)
}

/// The single verb an unattended scheduler snapshot is allowed to keep
/// beyond the baseline, if any.
///
/// `clawd::scheduler` refuses to create or re-arm a job unless the peer
/// can prove this verb or the owner approves a one-shot grant for it,
/// and the subsystem refuses to execute a snapshot that lacks it
/// (`cron::execute_job`, `triggers::execution_owner`). Re-admitting it
/// here is what keeps unattended work running; admitting the *other*
/// subsystem's verb would hand an agent turn a shell it was never
/// granted, so the mapping is one-to-one.
///
/// A fired trigger reaches this through the durable session it submits
/// its agent job with. A cron job runs a command rather than an agent
/// turn, so `cron::execute_job` registers its own process-registry
/// session from the same stored snapshot and never enters clawd scope;
/// the mapping is stated here so the two can never diverge.
fn delegated_executor_verb(origin: SessionOrigin) -> Option<Verb> {
    match origin {
        SessionOrigin::SystemAgentTask => None,
        SessionOrigin::CronDelegation => Some(Verb::PROC_SPAWN),
        SessionOrigin::TriggerDelegation => Some(Verb::AGENT_SPAWN),
    }
}

/// May this stored capability survive into an unattended execution?
///
/// Only two shapes qualify, and both are re-admitted verbatim from the
/// snapshot rather than reconstructed, so nothing here can widen a
/// scope:
///
/// * the subsystem's own executor verb, which addresses the session's
///   own children rather than a resource; and
/// * one credential named exactly as `cos cron add --credential` asked
///   for and the owner approved. A glob is refused, so a snapshot that
///   somehow carries `secret.read:**` grants nothing.
///
/// Everything else — filesystem, network, package/service/identity
/// mutation, devices, further delegation — is left to the baseline,
/// which means an unattended job can never persist privileged system
/// authority.
fn delegated_cap_is_admissible(cap: &Cap, executor: Verb) -> bool {
    if cap.verb == executor {
        return cap.scope == Scope::Wild;
    }
    if cap.verb == Verb::SECRET_READ {
        return matches!(&cap.scope, Scope::Name(name) if is_exact_secret_name(name));
    }
    false
}

/// An exact credential name: non-empty, no glob metacharacter, and no
/// traversal or separator games that a store lookup might normalize
/// into a different secret.
fn is_exact_secret_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('*')
        && !name.contains("..")
        && !name.starts_with('/')
        && !name.ends_with('/')
}

/// Authority that must never be delegated to a launcher the daemon
/// could not tie to a registered session, regardless of catalog risk.
///
/// Every verb here mutates machine-wide state or hands out credentials,
/// which is exactly the escalation an unauthenticated local process
/// would gain by asking for an App that declares it. Obtaining one of
/// these requires an authenticated parent session or an approved
/// permission grant.
const LOCAL_LAUNCH_DENIED_VERBS: &[Verb] = &[
    Verb::SYS_CONFIG,
    Verb::SYS_PACKAGE,
    Verb::SYS_IDENTITY,
    Verb::SYS_SERVICE,
    Verb::SYS_STORAGE,
    Verb::SYS_MOUNT,
    Verb::SYS_SNAPSHOT,
    Verb::SYS_SECURITY,
    Verb::SYS_POWER,
    Verb::NET_FIREWALL,
    Verb::NET_MANAGE,
    Verb::DATA_BACKUP,
    Verb::SECRET_READ,
    Verb::SECRET_WRITE,
    Verb::SECRET_GRANT,
];

/// Ceiling `clawd` delegates to a launcher it could not tie to a
/// registered session — the interactive `cos` CLI and the desktop
/// launcher.
///
/// This is a *policy* value owned by the daemon, not something a caller
/// may assert, and it is deliberately unprivileged: low/medium-risk
/// verbs only, never the machine-mutating set above, and never global
/// filesystem authority. Path scopes are bounded to the caller's own
/// home so an App launched without an authenticated parent cannot read
/// or write outside it. Anything beyond this ceiling has to come from
/// an authenticated parent session or an approved permission grant.
///
/// Unlike [`system_agent_caps`] this describes a process the user
/// started themselves, in their own session — not an Agent acting on
/// model output — so it stays risk-derived.
pub fn local_launcher_ceiling(owner_home: &Path) -> CapSet {
    let mut caps = CapSet::new();

    for meta in crate::caps::catalog::CATALOG {
        if meta.risk > Risk::Medium || LOCAL_LAUNCH_DENIED_VERBS.contains(&meta.verb) {
            continue;
        }
        let scope = match meta.scope_kind {
            ScopeKind::Path => Scope::path(format!("{}/**", owner_home.display())),
            ScopeKind::Host => Scope::host("**"),
            ScopeKind::Name => Scope::name("**"),
            // `SelfRef` addresses the session's own children and has no
            // narrower representable wildcard; `Wild`/`None` verbs carry
            // no resource at all.
            ScopeKind::SelfRef | ScopeKind::Wild | ScopeKind::None => Scope::Wild,
        };
        caps.insert(Cap::new(meta.verb, scope));
    }

    caps
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/system_caps.rs"
    ));
}

//! Which verified package a running instance came from, and what
//! happens to it when that package is revoked.
//!
//! Verifying at launch is not enough. An App session or MCP server can
//! run for hours; if the operator revokes the publisher key or the
//! artifact digest in that window, the already-running instance must
//! stop receiving authority rather than continue on the strength of a
//! check that passed before the revocation existed.
//!
//! # What is recorded
//!
//! Every launch records an [`Instance`] beside the session row: the
//! [`PackageRef`] it came from (kind, id, content digest, publisher key
//! id) and, once the child is bound, the exact process identity —
//! owner uid, pid, `/proc` start-time ticks and cgroup line. Under
//! `clawd` that file lives in `/run/cos/caps/<uid>`, which is
//! root-owned, so a session's package cannot be rewritten by the
//! session itself.
//!
//! # Timing guarantee
//!
//! Two mechanisms, and it is worth being exact about which one gives
//! which guarantee:
//!
//! * **Immediate, on use.** [`assert_live`] runs on the authority path
//!   — every capability decision, every worker broker request, every
//!   relayed route, every MCP tool call. It re-reads the trust store
//!   (which itself re-stats the durable generation and reloads when it
//!   moved) and denies before the call proceeds. So the *first* thing
//!   a revoked instance tries to do fails. This does not depend on any
//!   timer.
//! * **Bounded, for idle instances.** An instance that makes no
//!   authority call would otherwise sit there. [`lifecycle_tick`] runs
//!   from supervision loops that already exist — the `clawd` authority
//!   sweep and the `agentd` reconcile pass — and calls [`sweep`] plus
//!   [`enforce_shutdowns`]. The bound is the tick interval of whichever
//!   loop owns that view, not an instant.
//!
//! Neither path waits for a grant TTL and neither requires a daemon
//! restart.
//!
//! # Termination
//!
//! [`terminate`] kills the whole process *group*, because an App worker
//! and an MCP server both `setsid` at spawn and may have started
//! children of their own. Before signalling it re-reads the process
//! identity from `/proc` and refuses unless the uid, start time and
//! cgroup still match what was recorded — a pid that was recycled
//! after the instance exited names a different process and must not be
//! signalled. `SIGTERM`, a bounded grace, then `SIGKILL`.
//!
//! Skills have no process to kill: they fail their next catalog build
//! or disclosure call instead, which is the same guarantee in a
//! read-only shape.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::envelope::PackageKind;
use super::verify::{TrustSource, VerifiedPackage};
use super::TrustStore;

/// How long a revoked instance gets between `SIGTERM` and `SIGKILL`.
///
/// Short on purpose: this runs because the operator withdrew trust from
/// the code, so a slow, cooperative shutdown is not owed to it. Long
/// enough for a well-behaved server to close its transport and flush a
/// log line.
pub const SHUTDOWN_GRACE: Duration = Duration::from_millis(2000);

/// Poll interval while waiting for a signalled group to exit.
const REAP_POLL: Duration = Duration::from_millis(20);

/// The provenance of one running instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRef {
    pub kind: PackageKind,
    pub id: String,
    pub content_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_key_id: Option<String>,
    pub tier: String,
}

impl PackageRef {
    pub fn of(pkg: &VerifiedPackage) -> Self {
        Self {
            kind: pkg.kind(),
            id: pkg.id().to_string(),
            content_digest: pkg.content_digest().to_string(),
            publisher_key_id: match pkg.source() {
                TrustSource::Publisher { key_id } => Some(key_id.clone()),
                _ => None,
            },
            tier: pkg.tier().as_str().to_string(),
        }
    }

    /// Is this package still acceptable under `trust`?
    pub fn is_live(&self, trust: &TrustStore) -> Result<(), String> {
        if trust.is_package_revoked(&self.content_digest) {
            return Err(format!(
                "package `{}` ({}) was revoked by content digest",
                self.id, self.content_digest
            ));
        }
        if let Some(key_id) = &self.publisher_key_id {
            if trust.is_key_revoked(key_id) {
                return Err(format!(
                    "publisher key `{key_id}` for package `{}` was revoked",
                    self.id
                ));
            }
            let key = trust.key(key_id).ok_or_else(|| {
                format!(
                    "publisher key `{key_id}` for package `{}` is no longer trusted",
                    self.id
                )
            })?;
            if !key.usages.contains(super::trust::USAGE_PACKAGE_SIGNING)
                || !key.kinds.contains(&self.kind)
                || key.tier.as_str() != self.tier
                || !key.validity.contains(chrono::Utc::now())
            {
                return Err(format!(
                    "publisher key `{key_id}` no longer authorizes package `{}`",
                    self.id
                ));
            }
        } else if self.tier == super::TrustTier::Developer.as_str() {
            let grant = trust
                .dev_grant(self.kind, &self.id)
                .ok_or_else(|| format!("developer trust for package `{}` was removed", self.id))?;
            if grant.content_digest != self.content_digest {
                return Err(format!(
                    "developer trust for package `{}` no longer matches its content",
                    self.id
                ));
            }
        } else if self.tier != super::TrustTier::Vendor.as_str() {
            return Err(format!(
                "package `{}` has no publisher identity for trust tier `{}`",
                self.id, self.tier
            ));
        }
        Ok(())
    }
}

/// What kind of thing is running, and under which trust policy.
///
/// Classified explicitly rather than inferred from "does it have a
/// package?", so an operator-configured MCP server is a deliberate,
/// visible category instead of an unlabelled gap in the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceClass {
    /// A verified App package running as a sandboxed worker.
    App,
    /// A verified MCP/adapter package running as a sandboxed server.
    McpPackage,
    /// An MCP server the machine owner wrote into `config.json`.
    ///
    /// Not an installed package and never claimed to be one: there is
    /// no envelope, no publisher and nothing for a revocation to name.
    /// It is still sandboxed by the same worker policy and still
    /// recorded here, so `cos provenance` can say what is running and
    /// under which policy rather than leaving it invisible.
    McpOperatorConfig,
}

impl InstanceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::McpPackage => "mcp-package",
            Self::McpOperatorConfig => "mcp-operator-config",
        }
    }

    /// Is this instance governed by package provenance at all?
    pub fn is_package_backed(self) -> bool {
        !matches!(self, Self::McpOperatorConfig)
    }
}

/// The exact process an instance is, for signalling.
///
/// Every field is compared before a signal is sent. A pid on its own is
/// not an identity: the process it named can exit and the number be
/// handed to something else, and "terminate the revoked App" must never
/// become "terminate whatever holds that pid now".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessIdentity {
    pub uid: u32,
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`. Absent means the process could
    /// not be identified, and an unidentifiable process is never
    /// signalled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time_ticks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<String>,
}

impl ProcessIdentity {
    /// Read a live process's identity, or `None` if it cannot be named.
    pub fn of_process(uid: u32, pid: u32) -> Option<Self> {
        let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid)?;
        Some(Self {
            uid,
            pid,
            start_time_ticks: Some(start_time_ticks),
            cgroup: read_cgroup(pid),
        })
    }

    fn of_live_process(pid: u32) -> Option<Self> {
        Self::of_process(process_uid(pid)?, pid)
    }

    /// Is the process on the other end still the one that was recorded,
    /// and still actually running?
    ///
    /// Fails closed on every ambiguity: no recorded start time, no
    /// readable process, a different start time, a different uid, or a
    /// cgroup that both sides have and that disagrees.
    ///
    /// A zombie counts as gone. It still has a `/proc` entry and still
    /// answers `kill(pid, 0)`, but it executes nothing; treating it as
    /// alive would make every termination wait out the full grace and
    /// then send a pointless `SIGKILL`.
    pub fn still_matches(&self) -> bool {
        let Some(expected) = self.start_time_ticks else {
            return false;
        };
        if self.pid == 0 {
            return false;
        }
        if crate::proc::read_start_time_ticks_pub(self.pid) != Some(expected) {
            return false;
        }
        if is_zombie(self.pid) {
            return false;
        }
        if process_uid(self.pid) != Some(self.uid) {
            return false;
        }
        match (&self.cgroup, read_cgroup(self.pid)) {
            (Some(recorded), Some(current)) => recorded == &current,
            _ => true,
        }
    }
}

/// Has this process exited but not yet been reaped?
///
/// The state character is field 3 of `/proc/<pid>/stat`, but field 2 is
/// the executable name in parentheses and may itself contain spaces and
/// parentheses. Splitting after the *last* `)` is the only correct way
/// to reach the fields beyond it.
#[cfg(target_os = "linux")]
fn is_zombie(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
        return false;
    };
    matches!(rest.split_whitespace().next(), Some("Z") | Some("X"))
}

#[cfg(not(target_os = "linux"))]
fn is_zombie(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn read_cgroup(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let line = raw.lines().next()?.trim().to_string();
    (!line.is_empty() && line.len() <= 512).then_some(line)
}

#[cfg(not(target_os = "linux"))]
fn read_cgroup(_pid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
fn process_uid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(not(unix))]
fn process_uid(_pid: u32) -> Option<u32> {
    None
}

/// One running extension instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub class: InstanceClass,
    /// `None` only for [`InstanceClass::McpOperatorConfig`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageRef>,
    /// Present once the child has been spawned and bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessIdentity>,
    pub started_at: String,
}

impl Instance {
    /// Is this instance still acceptable under `trust`?
    ///
    /// An operator-configured MCP server has no package, so there is
    /// nothing provenance can revoke; it is governed by the fact that
    /// the owner wrote it into their own config and by the sandbox it
    /// runs in, which is a different policy and is labelled as one.
    pub fn is_live(&self, trust: &TrustStore) -> Result<(), String> {
        match &self.package {
            Some(reference) => reference.is_live(trust),
            None => Ok(()),
        }
    }
}

/// Why an instance is being wound down.
///
/// Carries the process identity as recorded, so the loop that acts on
/// it can prove it is signalling the same process rather than a pid
/// that has since been reused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    pub session_id: String,
    pub reason: String,
    pub marked_at: String,
    #[serde(default)]
    pub class: Option<InstanceClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RuntimeState {
    running: BTreeMap<String, Instance>,
    shutdown: BTreeMap<String, Shutdown>,
}

/// The canonical location of an owner's running-instance record.
///
/// One location, whoever is asking. The CLI running as the owner, the
/// `agentd` supervisor and `clawd` acting for that owner must all name
/// the same file, or a revocation recorded by one would be invisible to
/// the others — which is exactly the hole this record exists to close.
/// So the path is derived from an *explicit* owner uid rather than from
/// whatever `proc_data_dir()` happens to resolve to in the current
/// process or inside a `with_user_override` block.
///
/// `/run/cos/caps/<uid>` is the owner's root-owned routed partition:
/// `clawd` creates it `0750` owned by root with the owner's group, so
/// the owner can read their own record but cannot forge another's, and
/// cannot rewrite their own session's provenance. When that partition
/// does not exist — a developer running `cos` with no daemon — the
/// record falls back to the owner's own data directory, which only they
/// can write anyway.
pub fn state_path_for(owner: u32) -> PathBuf {
    if let Some(path) = std::env::var_os(STATE_DIR_ENV) {
        return PathBuf::from(path).join(STATE_FILE);
    }
    let routed = routed_root(owner);
    if routed.is_dir() {
        return routed.join(STATE_FILE);
    }
    // The fallback directory is not owner-partitioned the way the
    // routed one is, so the owner goes in the file name instead. Two
    // owners must never share a record even when they somehow share a
    // data directory.
    crate::paths::data_dir().join(format!("provenance-running.{owner}.json"))
}

fn routed_root(owner: u32) -> PathBuf {
    PathBuf::from("/run/cos/caps").join(owner.to_string())
}

fn lock_path_for(owner: u32) -> PathBuf {
    state_path_for(owner).with_extension("lock")
}

/// Test/CLI override for the directory holding the record.
///
/// Not a trust input: the record says *what is running*, never *what
/// may run*. Redirecting it can lose track of an instance, which fails
/// closed at every authority check that expects one, but it cannot make
/// an untrusted package trusted.
const STATE_DIR_ENV: &str = "COS_PROVENANCE_RUNTIME_DIR";
const STATE_FILE: &str = "provenance-running.json";

/// The owner this process is acting for.
///
/// Either the process's own euid, or — inside `clawd` — the uid the
/// daemon derived from the peer's kernel credentials and installed for
/// the duration of one authenticated request. Callers that already hold
/// an authenticated uid pass it explicitly instead of calling this.
pub fn current_owner() -> u32 {
    crate::paths::current_owner_uid_override().unwrap_or_else(super::fsec::effective_uid)
}

/// A held cross-process lock on one owner's record.
///
/// `flock` on a side file, not on the record itself, so the exclusion
/// survives the atomic rename that replaces it. Every read and every
/// mutation takes it, so two processes cannot interleave a
/// read-modify-write and lose one of the two updates — a launcher
/// registering an instance while a sweep marks another one is the
/// normal case, not a rare one.
struct StateLock {
    #[cfg(unix)]
    file: Option<std::fs::File>,
}

impl StateLock {
    fn acquire_shared(owner: u32) -> Result<Self, String> {
        Self::acquire_shared_at(&lock_path_for(owner))
    }

    fn acquire_shared_at(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;

            let file = match std::fs::OpenOptions::new().read(true).open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Self { file: None })
                }
                Err(error) => return Err(format!("open {}: {error}", path.display())),
            };
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
            if rc != 0 {
                return Err(format!(
                    "lock {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { file: Some(file) })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(Self {})
        }
    }

    fn acquire_exclusive(owner: u32) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            use std::os::unix::io::AsRawFd;

            let path = lock_path_for(owner);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)
                .map_err(|e| format!("open {}: {e}", path.display()))?;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "lock {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { file: Some(file) })
        }
        #[cfg(not(unix))]
        {
            let _ = owner;
            Ok(Self {})
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Some(file) = self.file.as_ref() {
                unsafe {
                    libc::flock(file.as_raw_fd(), libc::LOCK_UN);
                }
            }
        }
    }
}

/// Read one owner's record, validating the file it came from.
///
/// A missing file is an *empty* record, not an error: that is the state
/// before anything has launched. A file that exists but cannot be
/// trusted — wrong owner, group- or world-writable, a symlink, more
/// than one link, or unparseable — is an error, and every caller that
/// needs to know whether a session is live treats that error as a
/// denial rather than as "nothing recorded".
fn load_state(owner: u32) -> Result<RuntimeState, String> {
    load_state_at(owner, &state_path_for(owner))
}

fn load_state_at(owner: u32, path: &Path) -> Result<RuntimeState, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuntimeState::default())
        }
        Err(error) => return Err(format!("stat {}: {error}", path.display())),
    };
    validate_record_file(path, &meta, owner)?;
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| {
        format!(
            "the running-instance record at {} is corrupt: {error}",
            path.display()
        )
    })
}

/// Refuse a record file that something other than the owner or root
/// could have written.
fn validate_record_file(path: &Path, meta: &std::fs::Metadata, owner: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "the running-instance record at {} is a symlink",
                path.display()
            ));
        }
        if !meta.file_type().is_file() {
            return Err(format!(
                "the running-instance record at {} is not a regular file",
                path.display()
            ));
        }
        // More than one link means another directory can name this
        // inode, so the path is not the only way to reach it.
        if meta.nlink() != 1 {
            return Err(format!(
                "the running-instance record at {} has {} links",
                path.display(),
                meta.nlink()
            ));
        }
        if meta.uid() != 0 && meta.uid() != owner {
            return Err(format!(
                "the running-instance record at {} is owned by uid {}, not root or {owner}",
                path.display(),
                meta.uid()
            ));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(format!(
                "the running-instance record at {} is group/world writable ({:o})",
                path.display(),
                meta.mode() & 0o7777
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, meta, owner);
        Ok(())
    }
}

fn write_state(path: &Path, state: &RuntimeState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;

        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        let _ = super::fsec::sync_dir(parent);
    }
    Ok(())
}

/// Read one owner's record under the shared lock. Never writes.
///
/// A pure read that rewrote the file would be its own hazard: it would
/// take a write lock for every authority check, and a process holding a
/// stale in-memory copy would silently republish it over a newer one.
fn with_read<R>(owner: u32, f: impl FnOnce(&RuntimeState) -> R) -> Result<R, String> {
    let _lock = StateLock::acquire_shared(owner)?;
    let state = load_state(owner)?;
    Ok(f(&state))
}

/// Read-modify-write one owner's record atomically under the lock.
///
/// The read happens *inside* the lock, so the value being modified is
/// whatever the last writer left, not a copy this process cached
/// earlier. Two processes registering different instances therefore
/// both survive.
fn with_mutate<R>(owner: u32, f: impl FnOnce(&mut RuntimeState) -> R) -> Result<R, String> {
    let _lock = StateLock::acquire_exclusive(owner)?;
    // A corrupt record is not silently reset: overwriting it would
    // erase whatever instances it still described, and those are
    // exactly the ones a sweep would otherwise have stopped.
    let mut state = load_state(owner)?;
    let result = f(&mut state);
    let path = state_path_for(owner);
    write_state(&path, &state).map_err(|error| format!("persist {}: {error}", path.display()))?;
    if path.starts_with("/run/cos/caps") {
        crate::storage::refresh_routed_provenance_acl(owner)?;
    }
    Ok(result)
}

/// Mutate, logging rather than propagating. Used by the record-keeping
/// entry points, which have no caller to return an error to; the
/// *checking* entry points all propagate.
fn mutate_or_warn(owner: u32, f: impl FnOnce(&mut RuntimeState)) {
    if let Err(error) = with_mutate(owner, f) {
        tracing::error!(
            target: "provenance",
            owner,
            %error,
            "could not persist the running-instance record; \
             authority checks for this owner will fail closed"
        );
    }
}

/// Record that `session_id` is running `pkg`.
pub fn register(owner: u32, session_id: &str, pkg: &VerifiedPackage) {
    register_instance(
        owner,
        session_id,
        InstanceClass::App,
        Some(PackageRef::of(pkg)),
    );
}

/// Record a verified MCP/adapter package server.
pub fn register_mcp_package(owner: u32, session_id: &str, pkg: &VerifiedPackage) {
    register_instance(
        owner,
        session_id,
        InstanceClass::McpPackage,
        Some(PackageRef::of(pkg)),
    );
}

/// Record an MCP server that came from the owner's own configuration.
///
/// A separate kind on purpose: it has no package to revoke, but it is a
/// sandboxed child that the lifecycle loops should still be able to see
/// and account for, and a missing record for it must not be read as a
/// missing record for a *package* instance.
pub fn register_operator_mcp(owner: u32, session_id: &str) {
    register_instance(owner, session_id, InstanceClass::McpOperatorConfig, None);
}

fn register_instance(
    owner: u32,
    session_id: &str,
    class: InstanceClass,
    package: Option<PackageRef>,
) {
    if let Err(error) = register_instance_checked(owner, session_id, class, package) {
        tracing::error!(
            target: "provenance",
            owner,
            %error,
            "could not persist the running-instance record; \
             authority checks for this owner will fail closed"
        );
    }
}

fn register_instance_checked(
    owner: u32,
    session_id: &str,
    class: InstanceClass,
    package: Option<PackageRef>,
) -> Result<(), String> {
    let instance = Instance {
        class,
        package,
        process: None,
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    with_mutate(owner, |state| {
        state.shutdown.remove(session_id);
        state.running.insert(session_id.to_string(), instance);
    })
}

pub(crate) fn register_bound_instance(
    owner: u32,
    session_id: &str,
    class: InstanceClass,
    package: Option<PackageRef>,
) -> Result<(), String> {
    let valid = match (&class, &package) {
        (InstanceClass::App, Some(package)) => package.kind == PackageKind::App,
        (InstanceClass::McpPackage, Some(package)) => package.kind == PackageKind::Mcp,
        (InstanceClass::McpOperatorConfig, None) => true,
        _ => false,
    };
    if !valid {
        return Err("runtime instance class does not match its package identity".to_string());
    }
    register_instance_checked(owner, session_id, class, package)
}

/// Bind the spawned child to its instance record.
///
/// Called immediately after spawn, while the caller still holds the
/// unreaped child, so the identity read here is the identity of that
/// exact process and cannot already have been recycled.
pub fn bind_process(owner: u32, session_id: &str, pid: u32) {
    if let Err(error) = bind_process_checked(owner, session_id, pid) {
        tracing::error!(
            target: "provenance",
            owner,
            %error,
            "could not bind a running-instance process; \
             authority checks for this owner will fail closed"
        );
    }
}

pub(crate) fn bind_process_checked(owner: u32, session_id: &str, pid: u32) -> Result<(), String> {
    let identity = ProcessIdentity::of_live_process(pid)
        .ok_or_else(|| format!("process {pid} could not be identified for provenance"))?;
    let found = with_mutate(owner, |state| {
        let mut found = false;
        if let Some(instance) = state.running.get_mut(session_id) {
            instance.process = Some(identity.clone());
            found = true;
        }
        if let Some(shutdown) = state.shutdown.get_mut(session_id) {
            shutdown.process = Some(identity.clone());
            found = true;
        }
        found
    })?;
    if !found {
        return Err(format!(
            "running-instance record `{session_id}` disappeared before process bind"
        ));
    }
    let recorded = instance_for(owner, session_id)?
        .and_then(|instance| instance.process)
        .ok_or_else(|| format!("running-instance record `{session_id}` has no process"))?;
    if recorded != identity || !recorded.still_matches() {
        return Err(format!(
            "running-instance process for `{session_id}` could not be revalidated"
        ));
    }
    Ok(())
}

/// Forget a session that has exited.
pub fn deregister(owner: u32, session_id: &str) {
    if let Err(error) = deregister_checked(owner, session_id) {
        tracing::error!(
            target: "provenance",
            owner,
            %error,
            "could not remove the running-instance record"
        );
    }
}

pub(crate) fn deregister_checked(owner: u32, session_id: &str) -> Result<(), String> {
    with_mutate(owner, |state| {
        state.running.remove(session_id);
        state.shutdown.remove(session_id);
    })
}

/// The package a session is running, if any.
pub fn package_for(owner: u32, session_id: &str) -> Result<Option<PackageRef>, String> {
    with_read(owner, |state| {
        state
            .running
            .get(session_id)
            .and_then(|instance| instance.package.clone())
    })
}

/// The full instance record for a session.
pub fn instance_for(owner: u32, session_id: &str) -> Result<Option<Instance>, String> {
    with_read(owner, |state| state.running.get(session_id).cloned())
}

/// Every instance currently recorded, for diagnostics and sweeps.
pub fn running_instances(owner: u32) -> Result<BTreeMap<String, Instance>, String> {
    with_read(owner, |state| state.running.clone())
}

/// Re-check a running session's package against the current trust
/// store. Called on every authority decision, relay and tool call.
///
/// On failure the session is marked for bounded shutdown so the
/// supervisor stops it, and the caller is denied immediately — the
/// denial does not wait for the process to actually die.
///
/// A session with no record is *not* an extension instance and is
/// allowed: the operator's own shell and the daemon's own tasks pass
/// through here too. Callers that already know they are dealing with an
/// App or MCP session use [`assert_live_instance`], which refuses a
/// missing record instead.
pub fn assert_live(owner: u32, session_id: &str, trust: &TrustStore) -> Result<(), String> {
    check_live(owner, session_id, trust, false)
}

/// [`assert_live`], but a missing or unreadable record is a denial.
///
/// Used where the caller has independently established that the session
/// *is* a package-backed App or MCP session — a relay grant names one,
/// the session row carries an `app_id`. For those, "there is no record"
/// is not the same as "not an extension": it means the record was lost,
/// truncated or never written, and the one thing that would tell us
/// whether this instance's package is still trusted is missing. That
/// fails closed.
pub fn assert_live_instance(
    owner: u32,
    session_id: &str,
    trust: &TrustStore,
) -> Result<(), String> {
    check_live(owner, session_id, trust, true)
}

fn check_live(
    owner: u32,
    session_id: &str,
    trust: &TrustStore,
    require_record: bool,
) -> Result<(), String> {
    // A corrupt or unreadable record denies outright. It is the only
    // thing that could say this instance is still trusted, and a
    // failure to read it is not evidence that it is.
    let snapshot = with_read(owner, |state| {
        (
            state.shutdown.get(session_id).cloned(),
            state.running.get(session_id).cloned(),
        )
    })?;
    let (marked, instance) = snapshot;
    // A session already marked stays denied even if the mark has not
    // been acted on yet: the process may still be alive between the
    // mark and the next lifecycle pass, and it must not keep spending
    // authority in that window.
    if let Some(marked) = marked {
        return Err(marked.reason);
    }
    let Some(instance) = instance else {
        if require_record {
            return Err(format!(
                "no running-instance record for App session `{session_id}`; \
                 its package cannot be confirmed as still trusted"
            ));
        }
        return Ok(());
    };
    match instance.is_live(trust) {
        Ok(()) => Ok(()),
        Err(reason) => {
            mark_for_shutdown(owner, session_id, &reason);
            let package = instance.package.as_ref();
            super::audit(
                "provenance.revoked_instance_denied",
                serde_json::json!({
                    "session": session_id,
                    "class": instance.class.as_str(),
                    "package_kind": package.map(|p| p.kind.as_str()),
                    "package_id": package.map(|p| p.id.clone()),
                    "content_digest": package.map(|p| p.content_digest.clone()),
                    "publisher_key_id": package.and_then(|p| p.publisher_key_id.clone()),
                    "reason": reason,
                }),
            );
            Err(reason)
        }
    }
}

/// Assert liveness against a *freshly resolved* trust store.
///
/// The hot path for callers that are not already holding one. The
/// resolver re-stats the durable trust generation and rebuilds the
/// store when it moved, so a revocation written by another process
/// lands here without any notification, IPC or daemon restart.
pub fn assert_live_now(owner: u32, session_id: &str) -> Result<(), String> {
    assert_live(owner, session_id, &super::trust_store())
}

/// [`assert_live_instance`] against a freshly resolved trust store.
pub fn assert_live_instance_now(owner: u32, session_id: &str) -> Result<(), String> {
    assert_live_instance(owner, session_id, &super::trust_store())
}

pub fn assert_live_instance_for_owner_now(owner: u32, session_id: &str) -> Result<(), String> {
    assert_live_instance(owner, session_id, &super::trust_store_for_owner(owner))
}

pub fn assert_recorded_instance(owner: u32, session_id: &str) -> Result<(), String> {
    assert_recorded_snapshot(
        session_id,
        with_read(owner, |state| {
            (
                state.shutdown.get(session_id).cloned(),
                state.running.get(session_id).cloned(),
            )
        })?,
    )
}

pub fn assert_routed_recorded_instance(owner: u32, session_id: &str) -> Result<(), String> {
    let root = routed_root(owner);
    let state_path = root.join(STATE_FILE);
    let lock_path = state_path.with_extension("lock");
    let _lock = StateLock::acquire_shared_at(&lock_path)?;
    let metadata = std::fs::symlink_metadata(&state_path)
        .map_err(|error| format!("inspect routed running-instance record: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != 0 {
            return Err("routed running-instance record is not root-owned".to_string());
        }
    }
    let state = load_state_at(owner, &state_path)?;
    assert_recorded_snapshot(
        session_id,
        (
            state.shutdown.get(session_id).cloned(),
            state.running.get(session_id).cloned(),
        ),
    )
}

fn assert_recorded_snapshot(
    session_id: &str,
    (marked, running): (Option<Shutdown>, Option<Instance>),
) -> Result<(), String> {
    if let Some(marked) = marked {
        return Err(marked.reason);
    }
    let instance = running
        .ok_or_else(|| format!("no running-instance record for managed session `{session_id}`"))?;
    let process = instance.process.ok_or_else(|| {
        format!("managed session `{session_id}` has no recorded process identity")
    })?;
    if !process.still_matches() {
        return Err(format!(
            "managed session `{session_id}` no longer matches its recorded process"
        ));
    }
    Ok(())
}

/// The shutdown record for a session, if it has been marked.
pub fn shutdown_for(owner: u32, session_id: &str) -> Result<Option<Shutdown>, String> {
    with_read(owner, |state| state.shutdown.get(session_id).cloned())
}

/// Mark a session for bounded shutdown. Idempotent.
///
/// The process identity is copied from the running record at marking
/// time, so the record stays actionable even if the running entry is
/// cleared before the lifecycle pass gets to it.
pub fn mark_for_shutdown(owner: u32, session_id: &str, reason: &str) {
    mutate_or_warn(owner, |state| {
        let running = state.running.get(session_id).cloned();
        state
            .shutdown
            .entry(session_id.to_string())
            .or_insert_with(|| Shutdown {
                session_id: session_id.to_string(),
                reason: reason.to_string(),
                marked_at: chrono::Utc::now().to_rfc3339(),
                class: running.as_ref().map(|instance| instance.class),
                process: running
                    .as_ref()
                    .and_then(|instance| instance.process.clone()),
                content_digest: running
                    .as_ref()
                    .and_then(|instance| instance.package.as_ref())
                    .map(|package| package.content_digest.clone()),
            });
    });
}

/// Sessions the supervisor should terminate.
pub fn pending_shutdowns(owner: u32) -> Result<Vec<Shutdown>, String> {
    with_read(owner, |state| state.shutdown.values().cloned().collect())
}

/// Re-check every running instance against `trust`, marking the ones
/// whose package no longer verifies. Returns the newly marked sessions.
///
/// The supervisor calls this after any trust change so a revocation
/// reaches instances that are idle and would not otherwise make an
/// authority call.
pub fn sweep(owner: u32, trust: &TrustStore) -> Vec<Shutdown> {
    // One locked read-modify-write: the doomed set is computed from the
    // state that is about to be written back, so an instance registered
    // by another process between a read and a write cannot be dropped.
    let marked = with_mutate(owner, |state| {
        let doomed: Vec<(String, String)> = state
            .running
            .iter()
            .filter(|(session, _)| !state.shutdown.contains_key(*session))
            .filter_map(|(session, instance)| {
                instance
                    .is_live(trust)
                    .err()
                    .map(|reason| (session.clone(), reason))
            })
            .collect();
        let mut marked = Vec::new();
        for (session, reason) in doomed {
            let running = state.running.get(&session).cloned();
            let entry = Shutdown {
                session_id: session.clone(),
                reason: reason.clone(),
                marked_at: chrono::Utc::now().to_rfc3339(),
                class: running.as_ref().map(|instance| instance.class),
                process: running
                    .as_ref()
                    .and_then(|instance| instance.process.clone()),
                content_digest: running
                    .as_ref()
                    .and_then(|instance| instance.package.as_ref())
                    .map(|package| package.content_digest.clone()),
            };
            state.shutdown.insert(session, entry.clone());
            marked.push(entry);
        }
        marked
    });
    match marked {
        Ok(marked) => {
            for entry in &marked {
                super::audit(
                    "provenance.revoked_instance_marked",
                    serde_json::json!({
                        "session": entry.session_id,
                        "reason": entry.reason,
                    }),
                );
            }
            marked
        }
        Err(error) => {
            tracing::error!(
                target: "provenance",
                owner,
                %error,
                "could not sweep the running-instance record"
            );
            Vec::new()
        }
    }
}

/// How many package instances are running or awaiting shutdown.
///
/// Used by the consent guard: recording new developer trust while a
/// package is live would let running code influence the decision. An
/// unreadable record counts as "something is running", because the
/// guard's job is to refuse when it cannot prove otherwise.
pub fn pending_or_running(owner: u32) -> usize {
    with_read(owner, |state| state.running.len() + state.shutdown.len()).unwrap_or(usize::MAX)
}

/// What one lifecycle pass actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleReport {
    /// Sessions newly marked by this pass.
    pub marked: Vec<String>,
    /// Sessions whose process group was signalled and reaped.
    pub terminated: Vec<String>,
    /// Sessions whose record was cleared without signalling, because
    /// the recorded process was already gone or could no longer be
    /// proven to be the same one.
    pub released: Vec<String>,
}

impl LifecycleReport {
    pub fn is_empty(&self) -> bool {
        self.marked.is_empty() && self.terminated.is_empty() && self.released.is_empty()
    }

    /// Every session this pass finished with, whichever way.
    pub fn finished(&self) -> impl Iterator<Item = &String> {
        self.terminated.iter().chain(self.released.iter())
    }
}

/// Terminate one marked instance's process group.
///
/// Returns `Ok(true)` when a signal was actually delivered, `Ok(false)`
/// when the process was already gone or could not be proven to still be
/// the recorded one. Either way the runtime record is cleared, because
/// in both cases nothing of that instance is left to govern.
///
/// The identity check is the whole point: `pid` alone would let a
/// recycled number turn a revocation into a kill of somebody else's
/// process.
pub fn terminate(owner: u32, session_id: &str, grace: Duration) -> Result<bool, String> {
    let Some(marked) = shutdown_for(owner, session_id)? else {
        return Err(format!("session `{session_id}` is not marked for shutdown"));
    };
    let outcome = match marked.process.as_ref() {
        Some(identity) if identity.still_matches() => {
            let killed = signal_group(identity, grace);
            super::audit(
                "provenance.revoked_instance_terminated",
                serde_json::json!({
                    "session": session_id,
                    "class": marked.class.map(InstanceClass::as_str),
                    "content_digest": marked.content_digest,
                    "pid": identity.pid,
                    "signalled": killed,
                    "reason": marked.reason,
                }),
            );
            killed
        }
        Some(identity) => {
            // Recorded, but the process on the other end is not the one
            // that was recorded. It exited; the number may now belong to
            // something unrelated, so nothing is signalled.
            super::audit(
                "provenance.revoked_instance_released",
                serde_json::json!({
                    "session": session_id,
                    "pid": identity.pid,
                    "reason": "recorded process is gone or no longer matches",
                }),
            );
            false
        }
        None => false,
    };
    // Remove exactly this record, and only if it is still the one that
    // was acted on. A concurrent re-register of the same session id — a
    // relaunch after the kill — must not be erased by the pass that
    // stopped its predecessor.
    with_mutate(owner, |state| {
        let same_instance = state
            .shutdown
            .get(session_id)
            .map(|current| current.marked_at == marked.marked_at)
            .unwrap_or(false);
        if same_instance {
            state.shutdown.remove(session_id);
            let stale = state
                .running
                .get(session_id)
                .map(|instance| instance.started_at <= marked.marked_at)
                .unwrap_or(false);
            if stale {
                state.running.remove(session_id);
            }
        }
    })?;
    Ok(outcome)
}

/// `SIGTERM` the group, wait a bounded grace, then `SIGKILL` it.
///
/// The group, not the pid: an App worker and an MCP server each become
/// their own session leader at spawn, so their descendants share the
/// group and would otherwise outlive the parent. The group id is
/// re-read from the kernel and used only when the process really is its
/// own group leader, so this can never be aimed at an unrelated group.
#[cfg(unix)]
fn signal_group(identity: &ProcessIdentity, grace: Duration) -> bool {
    let pid = identity.pid as libc::pid_t;
    let pgid = unsafe { libc::getpgid(pid) };
    let target = if pgid == pid { -pgid } else { pid };
    unsafe {
        libc::kill(target, libc::SIGTERM);
    }
    if wait_for_exit(identity, grace) {
        return true;
    }
    unsafe {
        libc::kill(target, libc::SIGKILL);
    }
    wait_for_exit(identity, REAP_POLL * 25);
    true
}

#[cfg(not(unix))]
fn signal_group(_identity: &ProcessIdentity, _grace: Duration) -> bool {
    false
}

/// Poll until the recorded process is gone or `limit` elapses.
fn wait_for_exit(identity: &ProcessIdentity, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if !identity.still_matches() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(REAP_POLL);
    }
}

/// Act on every pending shutdown record.
///
/// This is the consumer [`mark_for_shutdown`] exists for. It is called
/// from supervision loops that already tick; it starts no loop of its
/// own and holds no thread when there is nothing to do.
pub fn enforce_shutdowns(owner: u32, grace: Duration) -> LifecycleReport {
    let mut report = LifecycleReport::default();
    let pending = match pending_shutdowns(owner) {
        Ok(pending) => pending,
        Err(error) => {
            tracing::error!(
                target: "provenance",
                owner,
                %error,
                "could not read pending provenance shutdowns"
            );
            return report;
        }
    };
    for marked in pending {
        let session = marked.session_id.clone();
        match terminate(owner, &session, grace) {
            Ok(true) => report.terminated.push(session),
            Ok(false) => report.released.push(session),
            Err(error) => {
                tracing::warn!(
                    target: "provenance",
                    session = %session,
                    %error,
                    "could not act on a pending provenance shutdown"
                );
            }
        }
    }
    report
}

/// One bounded lifecycle pass: re-check every instance, then act.
///
/// Called from the `clawd` authority sweep and the `agentd` reconcile
/// pass — loops that already exist and already tick. An instance that
/// never makes an authority call is caught here; one that does is
/// caught immediately by [`assert_live`], which is the stronger of the
/// two guarantees and does not depend on this running at all.
pub fn lifecycle_tick(owner: u32, trust: &TrustStore, grace: Duration) -> LifecycleReport {
    let marked = sweep(owner, trust);
    let mut report = enforce_shutdowns(owner, grace);
    report.marked = marked.into_iter().map(|entry| entry.session_id).collect();
    report
}

/// Kept for callers that used to drop a process-local cache.
///
/// There is no cache any more: every read loads and validates the
/// durable file under the shared lock, so nothing can go stale in
/// memory in the first place. This is a no-op, retained so the
/// supervision loops read as what they do rather than needing a comment
/// explaining an absence.
pub fn reset_cache() {}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/runtime.rs"
    ));
}

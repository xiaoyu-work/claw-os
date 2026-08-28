//! Approval queue — interactive consent for gated capability requests.
//!
//! When a gated operation is denied (or when an agent wants to
//! pre-emptively ask before attempting one), it writes a request to
//! `$COS_DATA_DIR/approvals/pending/<id>.json`. The Agent or desktop consent
//! surface presents the request in context and records the user's decision by
//! moving the file to `approved/` or `denied/`. The requester polls until the
//! file moves or the deadline passes.
//!
//! Layout:
//!
//! ```text
//! $COS_DATA_DIR/approvals/
//!     pending/<id>.json    # full Request
//!     approved/<id>.json   # Request + Outcome
//!     consumed/<id>.json   # approved once-grants after use
//!     denied/<id>.json
//! ```
//!
//! Rendering and notification are owned by the Agent UX. This module stores
//! durable consent evidence and atomically spends it. For supervised Agent
//! work, `clawd` then redeems that evidence into a one-use in-memory authority
//! grant bound to the live task and worker.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::caps::{Cap, ConsentContext, Risk, Scope, ScopeKind, Verb};

pub mod generations;

pub use generations::RevocationScope;

/// How long a grant lasts after the user approves it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantDuration {
    /// One-shot: the grant covers this single request and nothing more.
    Once,
    /// Lasts for the lifetime of the requesting session.
    Session,
    /// Persisted until the user revokes it; still bound to the approved request.
    Forever,
}

impl GrantDuration {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "once" | "one" | "1" => Some(Self::Once),
            "session" => Some(Self::Session),
            "forever" | "always" => Some(Self::Forever),
            _ => None,
        }
    }
}

/// What ends up in `pending/<id>.json` and gets carried forward
/// (unchanged) into `approved/<id>.json` or `denied/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub verb: String,
    pub scope: Scope,
    pub session: String,
    pub reason: String,
    pub requested_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,
    /// Catalog risk captured when the exact capability was requested.
    /// Missing on legacy records; those are re-derived before a new
    /// decision is allowed to mint authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    /// Execution context for Agent-originated consent. Non-Agent
    /// approval flows leave this absent and cannot satisfy an Agent
    /// capability denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ConsentContext>,
    /// Broker/process identity that originated an Agent request.
    /// Required whenever `context` is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ApprovalExecutionBinding>,
    /// Process that asked. Helps the user distinguish "the file
    /// manager I just opened" from "some background cron job."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<String>,
}

/// What the approver adds when they decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub outcome: Outcome,
    pub decided_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<GrantDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What the approval actually authorises.
    ///
    /// Absent on a record written before the capability authority
    /// existed. Such a record is evidence that a decision was made, but
    /// it carries no expiry, no use budget and no provenance, so it
    /// grants nothing: [`load_matching_grant`] refuses it. See
    /// [`GrantBinding`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantBinding>,
}

/// The bounded authority one approved record stands for.
///
/// A user saying "yes" to a capability is a decision about *that*
/// capability, not a standing licence, so every approval carries a
/// deadline, a use budget and a revocation generation even when the
/// user picked "for this session" or "always". `Once` spends exactly
/// one use; the longer choices bound the same grant by wall-clock time
/// and stay revocable through [`generations`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantBinding {
    /// Wall-clock deadline, seconds since the epoch.
    pub expires_at: u64,
    /// Uses the grant still has. `0` means spent.
    pub uses_remaining: u32,
    /// The revocation generation current for this owner and grant
    /// session when the approval was made.
    ///
    /// Compared against [`generations::current`] on every load, so an
    /// increment retires this grant immediately and a record restored
    /// from a backup taken before the increment stays dead. Optional on
    /// the wire and *required* in practice: a binding without one fails
    /// closed, because there is nothing to compare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u32>,
    /// Keyed, non-reversible reference shared with the audit trail.
    pub reference: String,
    /// Exact authority this decision may redeem.
    ///
    /// This is deliberately duplicated from the request. A restored or
    /// edited request cannot turn a historical decision into authority
    /// for a different owner, session, task, worker lease, capability,
    /// risk, or execution context. Bindings written before this field
    /// existed fail closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<ApprovalAuthorization>,
}

/// Exact capability and session context an approved record may redeem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalAuthorization {
    pub owner_uid: Option<u32>,
    pub session: String,
    pub capability: Cap,
    pub risk: Risk,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ConsentContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ApprovalExecutionBinding>,
}

/// Stable identity of one running Agent execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalExecutionIdentity {
    pub task_id: String,
    pub worker_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_start_time_ticks: Option<u64>,
    pub lease_nonce: String,
}

/// Request-time bounds attached to an Agent execution identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalExecutionBinding {
    #[serde(flatten)]
    pub identity: ApprovalExecutionIdentity,
    /// Wall-clock deadline inherited from the worker/process lease.
    pub expires_at: u64,
    /// Revocation generation when the request was filed.
    pub generation: u32,
}

/// A durable approval use that has been atomically spent.
///
/// For an Agent worker this is not yet execution authority. `clawd`
/// redeems it into a one-shot in-memory capability grant bound to the
/// authenticated task and worker before returning success to the gate.
#[derive(Debug, Clone)]
pub struct ConsumedGrant {
    pub duration: GrantDuration,
    pub expires_at: u64,
    pub uses_remaining: u32,
    pub generation: u32,
    pub reference: String,
    pub authorization: ApprovalAuthorization,
}

impl ConsumedGrant {
    pub fn expires_in(&self) -> Duration {
        let execution_expiry = self
            .authorization
            .execution
            .as_ref()
            .map(|execution| execution.expires_at)
            .unwrap_or(self.expires_at);
        Duration::from_secs(
            self.expires_at
                .min(execution_expiry)
                .saturating_sub(now_secs()),
        )
    }
}

/// Longest a `session`-scoped approval may stand.
const SESSION_GRANT_SECS: u64 = 8 * 60 * 60;
/// Longest a `forever` approval may stand before the user is asked
/// again. "Forever" is a UX promise about not being re-prompted during
/// ordinary use, not a promise that authority never expires.
const FOREVER_GRANT_SECS: u64 = 30 * 24 * 60 * 60;
/// Uses a non-one-shot approval carries before it must be renewed.
const REPEATABLE_GRANT_USES: u32 = 512;
/// A local attended process has this long to approve and retry.
const LOCAL_EXECUTION_SECS: u64 = 15 * 60;
/// Matches the maximum accepted `claw-agentd` lease.
const MAX_EXECUTION_BINDING_SECS: u64 = 24 * 60 * 60;

tokio::task_local! {
    static LOCAL_EXECUTION: LocalExecutionContext;
}

#[derive(Clone)]
struct LocalExecutionContext {
    identity: ApprovalExecutionIdentity,
    touched: Arc<AtomicBool>,
}

/// One in-process Agent invocation.
///
/// Multiplexed runtimes must create one of these for every conversation turn
/// and scope the complete model/tool future with [`Self::scope`]. Dropping the
/// invocation retires every pending or approved record carrying its fresh
/// nonce, including cancellation and client-disconnect paths.
pub(crate) struct LocalApprovalInvocation {
    context: LocalExecutionContext,
}

impl LocalApprovalInvocation {
    pub(crate) fn new(task_id: impl Into<String>) -> Result<Self, String> {
        let task_id = task_id.into();
        if task_id.is_empty() || task_id.len() > 128 || task_id.chars().any(char::is_control) {
            return Err("local approval task id is invalid".to_string());
        }
        let worker_pid = std::process::id();
        let worker_start_time_ticks = crate::proc::read_start_time_ticks_pub(worker_pid);
        if cfg!(target_os = "linux") && worker_start_time_ticks.is_none() {
            return Err("local approval process identity is not verifiable".to_string());
        }
        Ok(Self {
            context: LocalExecutionContext {
                identity: ApprovalExecutionIdentity {
                    task_id,
                    worker_pid,
                    worker_start_time_ticks,
                    lease_nonce: uuid::Uuid::new_v4().to_string(),
                },
                touched: Arc::new(AtomicBool::new(false)),
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn identity(&self) -> &ApprovalExecutionIdentity {
        &self.context.identity
    }

    pub(crate) async fn scope<F>(self, future: F) -> F::Output
    where
        F: std::future::Future,
    {
        let context = self.context.clone();
        LOCAL_EXECUTION
            .scope(context, async move {
                let _invocation = self;
                future.await
            })
            .await
    }

    #[cfg(test)]
    pub(crate) fn sync_scope<F, R>(self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let context = self.context.clone();
        LOCAL_EXECUTION.sync_scope(context, || {
            let _invocation = self;
            f()
        })
    }
}

impl Drop for LocalApprovalInvocation {
    fn drop(&mut self) {
        if !self.context.touched.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Err(error) =
            invalidate_local_execution(&self.context.identity, "local Agent invocation ended")
        {
            tracing::error!(
                task_id = %self.context.identity.task_id,
                error = %error,
                "failed to retire local Agent approval state"
            );
        }
    }
}

#[derive(Clone)]
pub(crate) struct CapturedLocalExecution(LocalExecutionContext);

pub(crate) fn capture_local_execution() -> Option<CapturedLocalExecution> {
    LOCAL_EXECUTION
        .try_with(|context| CapturedLocalExecution(context.clone()))
        .ok()
}

pub(crate) fn with_captured_local_execution<F, R>(
    captured: Option<CapturedLocalExecution>,
    f: F,
) -> R
where
    F: FnOnce() -> R,
{
    match captured {
        Some(CapturedLocalExecution(context)) => LOCAL_EXECUTION.sync_scope(context, f),
        None => f(),
    }
}

impl ApprovalExecutionBinding {
    pub fn for_worker(
        task_id: impl Into<String>,
        worker_pid: u32,
        worker_start_time_ticks: Option<u64>,
        lease_nonce: impl Into<String>,
        expires_at: u64,
        owner_uid: Option<u32>,
        session: &str,
    ) -> Result<Self, String> {
        let task_id = task_id.into();
        let lease_nonce = lease_nonce.into();
        if task_id.is_empty() || task_id.len() > 128 {
            return Err("approval task id is invalid".to_string());
        }
        if worker_pid == 0 || worker_start_time_ticks.is_none() {
            return Err("approval worker identity is not verifiable".to_string());
        }
        if crate::proc::read_start_time_ticks_pub(worker_pid) != worker_start_time_ticks {
            return Err("approval worker start time does not match the live process".to_string());
        }
        if lease_nonce.len() < 16 || lease_nonce.len() > 128 {
            return Err("approval lease nonce is invalid".to_string());
        }
        let now = now_secs();
        if expires_at <= now {
            return Err("approval request lease has expired".to_string());
        }
        if expires_at > now.saturating_add(MAX_EXECUTION_BINDING_SECS) {
            return Err("approval request lease exceeds the maximum lifetime".to_string());
        }
        Ok(Self {
            identity: ApprovalExecutionIdentity {
                task_id,
                worker_pid,
                worker_start_time_ticks,
                lease_nonce,
            },
            expires_at,
            generation: generations::current(owner_uid, session)?,
        })
    }

    fn is_live_for(
        &self,
        expected: &ApprovalExecutionIdentity,
        owner_uid: Option<u32>,
        session: &str,
        now: u64,
    ) -> bool {
        if self.identity.task_id.is_empty()
            || self.identity.task_id.len() > 128
            || self.identity.lease_nonce.len() < 16
            || self.identity.lease_nonce.len() > 128
            || &self.identity != expected
            || now >= self.expires_at
            || execution_is_revoked(&self.identity)
        {
            return false;
        }
        match self.identity.worker_start_time_ticks {
            Some(expected_start)
                if crate::proc::read_start_time_ticks_pub(self.identity.worker_pid)
                    != Some(expected_start) =>
            {
                return false;
            }
            None if cfg!(target_os = "linux") => return false,
            _ => {}
        }
        matches!(
            generations::current(owner_uid, session),
            Ok(current) if current == self.generation
        )
    }
}

fn local_execution_identity() -> Result<ApprovalExecutionIdentity, String> {
    LOCAL_EXECUTION
        .try_with(|context| {
            context.touched.store(true, Ordering::SeqCst);
            context.identity.clone()
        })
        .map_err(|_| {
            "Agent approval requires an active per-invocation execution identity".to_string()
        })
}

fn local_execution_binding(
    owner_uid: Option<u32>,
    session: &str,
) -> Result<ApprovalExecutionBinding, String> {
    let identity = local_execution_identity()?;
    let expires_at = now_secs().saturating_add(LOCAL_EXECUTION_SECS);
    let generation = generations::current(owner_uid, session)?;
    Ok(ApprovalExecutionBinding {
        identity,
        expires_at,
        generation,
    })
}

impl GrantBinding {
    fn mint(
        duration: GrantDuration,
        id: &str,
        now: u64,
        generation: u32,
        authorization: ApprovalAuthorization,
    ) -> Self {
        let (lifetime, uses) = match duration {
            GrantDuration::Once => (SESSION_GRANT_SECS, 1),
            GrantDuration::Session => (SESSION_GRANT_SECS, REPEATABLE_GRANT_USES),
            GrantDuration::Forever => (FOREVER_GRANT_SECS, REPEATABLE_GRANT_USES),
        };
        Self {
            expires_at: now.saturating_add(lifetime),
            uses_remaining: uses,
            generation: Some(generation),
            reference: crate::audit_policy::text_digest(id).digest,
            authorization: Some(authorization),
        }
    }

    /// Is this binding still authority for the exact expected request?
    ///
    /// Three independent conditions, each of which alone kills the
    /// grant: the use budget, the deadline, and the revocation
    /// generation. The generation lookup reads root-owned state, and an
    /// error there is a refusal — an authority that cannot tell whether
    /// something was revoked must assume it was.
    fn is_live(
        &self,
        now: u64,
        expected: &ApprovalAuthorization,
        expected_execution: Option<&ApprovalExecutionIdentity>,
    ) -> bool {
        if self.uses_remaining == 0 || now >= self.expires_at {
            return false;
        }
        let Some(authorization) = self.authorization.as_ref() else {
            return false;
        };
        if authorization.owner_uid != expected.owner_uid
            || authorization.session != expected.session
            || authorization.capability != expected.capability
            || authorization.risk != expected.risk
            || authorization.context != expected.context
        {
            return false;
        }
        match (authorization.execution.as_ref(), expected_execution) {
            (Some(execution), Some(expected)) => {
                if !execution.is_live_for(
                    expected,
                    authorization.owner_uid,
                    &authorization.session,
                    now,
                ) {
                    return false;
                }
            }
            (None, None) => {}
            _ => return false,
        }
        let Some(generation) = self.generation else {
            // Written before revocation generations existed. The
            // decision was real; the standing authority it implies was
            // never bounded, so it is refused until re-approved.
            return false;
        };
        match generations::current(authorization.owner_uid, &authorization.session) {
            Ok(current) => generation == current,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    "approval revocation state is unreadable; refusing the grant"
                );
                false
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Approved,
    Denied,
}

/// Combined record persisted to `approved/` or `denied/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolved {
    #[serde(flatten)]
    pub request: Request,
    pub decision: Decision,
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

fn root() -> PathBuf {
    crate::paths::caps_data_dir().join("approvals")
}

fn pending_dir() -> PathBuf {
    root().join("pending")
}
fn approved_dir() -> PathBuf {
    root().join("approved")
}
fn denied_dir() -> PathBuf {
    root().join("denied")
}
fn consumed_dir() -> PathBuf {
    root().join("consumed")
}
fn revoked_execution_dir() -> PathBuf {
    root().join("revoked-executions")
}
/// Holding area for requests that have been claimed by a resolver
/// (atomically renamed out of `pending/`) but not yet written to
/// `approved/` or `denied/`. A crash between the claim and the final
/// write leaves an orphan here; that is acceptable because the
/// alternative is the request being silently lost or — worse — being
/// resolved by two callers simultaneously.
fn scratch_dir() -> PathBuf {
    root().join("scratch")
}

fn ensure_dirs() -> std::io::Result<()> {
    crate::storage::ensure_private_dir(&pending_dir())?;
    crate::storage::ensure_private_dir(&approved_dir())?;
    crate::storage::ensure_private_dir(&denied_dir())?;
    crate::storage::ensure_private_dir(&consumed_dir())?;
    crate::storage::ensure_private_dir(&revoked_execution_dir())?;
    crate::storage::ensure_private_dir(&scratch_dir())?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn short_id() -> String {
    // 12 hex chars from a timestamp + entropy enough to avoid clashes
    // within the same session. We don't need cryptographic strength.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    std::process::id().hash(&mut h);
    format!("{:012x}", h.finish() & 0xFFFFFFFFFFFF)
}

fn execution_revocation_path(identity: &ApprovalExecutionIdentity) -> PathBuf {
    let material = format!(
        "{}\0{}\0{}\0{}",
        identity.task_id,
        identity.worker_pid,
        identity.worker_start_time_ticks.unwrap_or_default(),
        identity.lease_nonce
    );
    revoked_execution_dir().join(format!(
        "{}.json",
        crate::crypto::sha256_hex(material.as_bytes())
    ))
}

fn execution_is_revoked(identity: &ApprovalExecutionIdentity) -> bool {
    match fs::metadata(execution_revocation_path(identity)) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::error!(
                task_id = %identity.task_id,
                error = %error,
                "approval execution revocation state is unreadable; refusing the grant"
            );
            true
        }
    }
}

fn revoke_execution(identity: &ApprovalExecutionIdentity) -> Result<(), String> {
    ensure_dirs().map_err(|error| format!("approvals dir: {error}"))?;
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "revoked_at": now_secs(),
    }))
    .map_err(|error| error.to_string())?;
    write_atomic_with(
        &execution_revocation_path(identity),
        &payload,
        Durability::Committed,
    )
    .map_err(|error| format!("persist execution revocation: {error}"))
}

fn validate_approval_id(id: &str) -> Result<(), String> {
    let Some(suffix) = id.strip_prefix("ap-") else {
        return Err(format!("invalid approval id: {id}"));
    };
    if suffix.len() != 12
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("invalid approval id: {id}"));
    }
    Ok(())
}

/// Atomically write `data` to `path`. Writes go through a sibling
/// `.tmp.<nonce>` file, are fsynced, and are then renamed over the
/// final path. On Linux + POSIX this guarantees a reader sees either
/// the previous bytes or the new bytes — never a truncated payload.
///
/// Why we need this: the approval queue is the trust boundary
/// between a gated agent action and the user's consent. A partial
/// write of `pending/<id>.json` followed by a process kill would
/// leave a non-parseable file; the read side filters parse errors
/// silently, so the user's request would just disappear. With the
/// tmp-write + rename pattern that scenario is impossible.
///
/// `durability` decides what happens to the parent-directory `fsync`
/// that commits the rename itself. Most records here are re-derivable
/// or re-requestable, so losing one to a power cut is recoverable and
/// the sync is advisory. A revocation counter is not: losing its
/// increment silently re-arms authority the user retired, so
/// [`Durability::Committed`] makes the directory sync mandatory and
/// reports failure rather than success.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, data, Durability::BestEffort)
}

/// How firmly a write must be committed before it may be called done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Durability {
    /// The directory entry is synced if the kernel lets us, and a
    /// failure is ignored. Correct for records whose loss is
    /// recoverable by asking again.
    BestEffort,
    /// The directory entry must be durably committed. A failure is
    /// returned, so the caller cannot report success for a change that
    /// may not have survived.
    Committed,
}

fn write_atomic_with(path: &Path, data: &[u8], durability: Durability) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    crate::storage::ensure_private_dir(parent)?;

    let leaf = path.file_name().and_then(|s| s.to_str()).unwrap_or("anon");
    // Hidden tmp name so partial writes never appear in directory
    // listings. The .tmp suffix + nonce keep concurrent writers from
    // racing on the same scratch path.
    let tmp_path = parent.join(format!(".{leaf}.tmp.{}", short_id()));

    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp_path)?;
        f.write_all(data)?;
        // fsync the data + metadata of the tmp file before linking
        // it into place under the user-visible name.
        f.sync_all()?;
    }

    // Atomic rename is unconditional and overwrites the destination
    // on POSIX. After this call a reader sees the new bytes; before
    // it, the old bytes.
    if let Err(e) = fs::rename(&tmp_path, path) {
        // Best-effort cleanup — the tmp file is hidden, so leaving
        // it behind is at worst a tiny disk-space leak.
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    crate::storage::set_private_file(path)?;

    // Commit the directory entry the rename created. Without this the
    // new bytes are on disk but the name may still point at the old
    // inode after a power cut, on filesystems where directory metadata
    // is not auto-flushed (ext4 with `data=writeback`, xfs, …).
    match durability {
        Durability::BestEffort => {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Durability::Committed => {
            let dir = fs::File::open(parent)?;
            dir.sync_all()?;
            // A test hook, compiled out of release builds, so the
            // failure path can be exercised without an unmountable
            // filesystem.
            #[cfg(test)]
            fail_parent_sync_if_requested()?;
        }
    }
    Ok(())
}

/// Test-only injection point for a parent-directory sync failure.
#[cfg(test)]
fn fail_parent_sync_if_requested() -> std::io::Result<()> {
    if PARENT_SYNC_FAILS.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other(
            "injected parent directory sync failure",
        ));
    }
    Ok(())
}

#[cfg(test)]
static PARENT_SYNC_FAILS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn set_parent_sync_failure(fails: bool) {
    PARENT_SYNC_FAILS.store(fails, std::sync::atomic::Ordering::SeqCst);
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

pub fn canonical_capability(verb: Verb, scope: Scope) -> Result<(Cap, Risk), String> {
    let meta = crate::caps::lookup_meta(verb)
        .ok_or_else(|| format!("capability verb is not in the catalog: {}", verb.as_str()))?;
    let scope = scope.canonicalized();
    let kind_matches = matches!(
        (meta.scope_kind, &scope),
        (_, Scope::Wild)
            | (ScopeKind::Path, Scope::Path(_))
            | (ScopeKind::Host, Scope::Host(_))
            | (ScopeKind::Name, Scope::Name(_))
            | (ScopeKind::SelfRef, Scope::SelfRef(_))
    );
    if !kind_matches {
        return Err(format!(
            "capability {} expects {:?} scope, got {:?}",
            verb.as_str(),
            meta.scope_kind,
            scope.kind()
        ));
    }
    let rendered = scope.to_string();
    if rendered.is_empty() || rendered.len() > 512 || rendered.contains(['\n', '\r', '\0']) {
        return Err("capability scope is not a bounded single-line value".to_string());
    }
    Ok((Cap::new(verb, scope), meta.risk))
}

pub fn capability_risk(verb: Verb, scope: &Scope) -> Result<Risk, String> {
    canonical_capability(verb, scope.clone()).map(|(_, risk)| risk)
}

fn authorization_for_request(request: &Request) -> Result<ApprovalAuthorization, String> {
    let verb = Verb::parse(&request.verb)
        .ok_or_else(|| format!("unknown capability verb: {}", request.verb))?;
    let (capability, risk) = canonical_capability(verb, request.scope.clone())?;
    if capability.scope != request.scope {
        return Err(
            "approval request scope is not canonical; request a fresh approval".to_string(),
        );
    }
    if request.risk.is_some_and(|recorded| recorded != risk) {
        return Err(format!(
            "capability risk changed for {}; request a fresh approval",
            request.verb
        ));
    }
    match (request.context, request.execution.as_ref()) {
        (Some(_), Some(execution)) => {
            if !execution.is_live_for(
                &execution.identity,
                request.owner_uid,
                &request.session,
                now_secs(),
            ) {
                return Err("approval request no longer matches a live execution".to_string());
            }
        }
        (Some(_), None) => {
            return Err("Agent approval request has no execution binding".to_string());
        }
        (None, Some(_)) => {
            return Err("non-Agent approval request carries an execution binding".to_string());
        }
        (None, None) => {}
    }
    Ok(ApprovalAuthorization {
        owner_uid: request.owner_uid,
        session: request.session.clone(),
        capability,
        risk,
        context: request.context,
        execution: request.execution.clone(),
    })
}

fn validate_agent_duration(
    authorization: &ApprovalAuthorization,
    duration: GrantDuration,
) -> Result<(), String> {
    if authorization.context != Some(ConsentContext::Attended) {
        return Ok(());
    }
    match (authorization.risk, duration) {
        (Risk::Critical, GrantDuration::Session | GrantDuration::Forever) => Err(
            "critical Agent capabilities may only be approved once; choose duration `once`"
                .to_string(),
        ),
        (Risk::High, GrantDuration::Forever) => Err(
            "high-risk Agent capabilities may not be approved forever; choose `once` or `session`"
                .to_string(),
        ),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Public API — write side
// ---------------------------------------------------------------------------

/// Submit a pending approval request. Returns the request id the
/// caller can use with [`wait`].
pub fn submit(
    verb: Verb,
    scope: Scope,
    session: impl Into<String>,
    reason: impl Into<String>,
    requester: Option<String>,
) -> Result<String, String> {
    submit_owned(verb, scope, session, reason, requester, None)
}

pub fn submit_owned(
    verb: Verb,
    scope: Scope,
    session: impl Into<String>,
    reason: impl Into<String>,
    requester: Option<String>,
    owner_uid: Option<u32>,
) -> Result<String, String> {
    submit_owned_with_context(verb, scope, session, reason, requester, owner_uid, None)
}

/// Submit an Agent-originated capability request with its trusted
/// attended/unattended context.
pub fn submit_owned_with_context(
    verb: Verb,
    scope: Scope,
    session: impl Into<String>,
    reason: impl Into<String>,
    requester: Option<String>,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
) -> Result<String, String> {
    let session = session.into();
    if context == Some(ConsentContext::Unattended) {
        return Err("unattended execution cannot create an approval request".to_string());
    }
    let execution = match context {
        Some(_) => Some(local_execution_binding(owner_uid, &session)?),
        None => None,
    };
    submit_owned_with_execution(
        verb, scope, session, reason, requester, owner_uid, context, execution,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn submit_worker_request(
    verb: Verb,
    scope: Scope,
    session: impl Into<String>,
    reason: impl Into<String>,
    requester: Option<String>,
    owner_uid: u32,
    task_id: impl Into<String>,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    lease_nonce: impl Into<String>,
    expires_at: u64,
) -> Result<String, String> {
    let session = session.into();
    let execution = ApprovalExecutionBinding::for_worker(
        task_id,
        worker_pid,
        worker_start_time_ticks,
        lease_nonce,
        expires_at,
        Some(owner_uid),
        &session,
    )?;
    submit_owned_with_execution(
        verb,
        scope,
        session,
        reason,
        requester,
        Some(owner_uid),
        Some(ConsentContext::Attended),
        Some(execution),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn submit_owned_with_execution(
    verb: Verb,
    scope: Scope,
    session: impl Into<String>,
    reason: impl Into<String>,
    requester: Option<String>,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
    execution: Option<ApprovalExecutionBinding>,
) -> Result<String, String> {
    let session = session.into();
    if context == Some(ConsentContext::Unattended) {
        return Err("unattended execution cannot create an approval request".to_string());
    }
    if context.is_some() != execution.is_some() {
        return Err(
            "Agent consent context and execution binding must be provided together".to_string(),
        );
    }
    if let Some(execution) = execution.as_ref() {
        if !execution.is_live_for(&execution.identity, owner_uid, &session, now_secs()) {
            return Err("approval request execution binding is not live".to_string());
        }
    }
    let (capability, risk) = canonical_capability(verb, scope)?;
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    let req = Request {
        id: format!("ap-{}", short_id()),
        verb: verb.as_str().to_string(),
        scope: capability.scope,
        session,
        reason: reason.into(),
        requested_at: now_secs(),
        owner_uid,
        risk: Some(risk),
        context,
        execution,
        requester,
    };
    let path = pending_dir().join(format!("{}.json", req.id));
    let data = serde_json::to_string_pretty(&req).map_err(|e| e.to_string())?;
    write_atomic(&path, data.as_bytes()).map_err(|e| format!("write pending: {e}"))?;
    crate::clawd::system_journal::record_approval_request(&req);
    Ok(req.id)
}

pub fn approve(
    id: &str,
    duration: GrantDuration,
    decided_by: Option<String>,
    note: Option<String>,
) -> Result<Resolved, String> {
    resolve(
        id,
        Outcome::Approved,
        Some(duration),
        decided_by,
        note,
        None,
    )
}

pub fn deny(
    id: &str,
    decided_by: Option<String>,
    note: Option<String>,
) -> Result<Resolved, String> {
    resolve(id, Outcome::Denied, None, decided_by, note, None)
}

pub fn approve_for_owner(
    id: &str,
    duration: GrantDuration,
    decided_by: Option<String>,
    note: Option<String>,
    owner_uid: Option<u32>,
) -> Result<Resolved, String> {
    resolve(
        id,
        Outcome::Approved,
        Some(duration),
        decided_by,
        note,
        owner_uid,
    )
}

pub fn deny_for_owner(
    id: &str,
    decided_by: Option<String>,
    note: Option<String>,
    owner_uid: Option<u32>,
) -> Result<Resolved, String> {
    resolve(id, Outcome::Denied, None, decided_by, note, owner_uid)
}

fn resolve(
    id: &str,
    outcome: Outcome,
    duration: Option<GrantDuration>,
    decided_by: Option<String>,
    note: Option<String>,
    owner_uid: Option<u32>,
) -> Result<Resolved, String> {
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    crate::filelock::with_exclusive_path_lock(&grant_lock_path(), || {
        resolve_locked(id, outcome, duration, decided_by, note, owner_uid)
    })
}

fn resolve_locked(
    id: &str,
    outcome: Outcome,
    duration: Option<GrantDuration>,
    decided_by: Option<String>,
    note: Option<String>,
    owner_uid: Option<u32>,
) -> Result<Resolved, String> {
    validate_approval_id(id)?;
    let pending = pending_dir().join(format!("{id}.json"));
    if let Some(uid) = owner_uid {
        let request =
            lookup_pending(id).ok_or_else(|| format!("no pending request with id `{id}`"))?;
        if request.owner_uid != Some(uid) {
            return Err(format!("permission request is not owned by uid {uid}"));
        }
    }

    // Atomically claim the request: rename `pending/<id>.json` out of
    // the pending directory into our process-private scratch path.
    // POSIX rename is atomic, so concurrent resolvers (CLI + GUI
    // applet, two reviewers, …) see exactly ONE winner — the one
    // whose rename succeeded. Everyone else gets ENOENT and a clean
    // "no pending request" error.
    //
    // The scratch path includes a per-resolver nonce so two simultaneous
    // resolvers do not race on the same destination either.
    let scratch = scratch_dir().join(format!("{id}.{}.json", short_id()));
    if let Some(parent) = scratch.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("scratch dir: {e}"))?;
    }
    match fs::rename(&pending, &scratch) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no pending request with id `{id}`"));
        }
        Err(e) => return Err(format!("claim pending {id}: {e}")),
    }

    // From here on we exclusively own `scratch`. A crash between this
    // point and the final write leaves an orphan file in scratch_dir
    // that can be inspected post-mortem; correctness is preserved
    // (the request is not in pending/, not in approved/, not in
    // denied/ — it is "in flight" and recoverable by hand).
    let data = match fs::read_to_string(&scratch) {
        Ok(s) => s,
        Err(e) => return Err(format!("read claimed {id}: {e}")),
    };
    let mut request: Request =
        serde_json::from_str(&data).map_err(|e| format!("parse claimed {id}: {e}"))?;
    if !request_visible_to(&request, owner_uid) {
        fs::rename(&scratch, &pending)
            .map_err(|e| format!("restore unauthorized pending {id}: {e}"))?;
        return Err(format!(
            "permission request is not owned by uid {}",
            owner_uid.expect("owner filter is set when visibility fails")
        ));
    }

    let decided_at = now_secs();
    let authorization = if outcome == Outcome::Approved {
        let authorization = match authorization_for_request(&request) {
            Ok(authorization) => authorization,
            Err(error) => {
                invalidate_claimed_request(id, &scratch, &request, &error)?;
                return Err(error);
            }
        };
        if let Some(duration) = duration {
            if let Err(error) = validate_agent_duration(&authorization, duration) {
                fs::rename(&scratch, &pending)
                    .map_err(|restore| format!("{error}; restore pending {id}: {restore}"))?;
                return Err(error);
            }
        }
        request.risk = Some(authorization.risk);
        Some(authorization)
    } else {
        None
    };
    // The generation is captured from root-owned state at decision
    // time, so a later revocation of this owner or this grant session
    // retires the record without touching it — and a restore of this
    // very file from an older backup cannot bring it back.
    let generation = match outcome {
        Outcome::Approved => match generations::current(request.owner_uid, &request.session) {
            Ok(generation)
                if authorization
                    .as_ref()
                    .and_then(|authorization| authorization.execution.as_ref())
                    .is_none_or(|execution| execution.generation == generation) =>
            {
                Some(generation)
            }
            Ok(_) => {
                let message = "approval request generation changed before the decision".to_string();
                invalidate_claimed_request(id, &scratch, &request, &message)?;
                return Err(message);
            }
            Err(error) => {
                let message = format!("could not read approval revocation state: {error}");
                invalidate_claimed_request(id, &scratch, &request, &message)?;
                return Err(message);
            }
        },
        Outcome::Denied => None,
    };
    let decision = Decision {
        outcome,
        decided_at,
        decided_by,
        duration,
        note,
        // Only an approval carries authority, and only a bounded one.
        grant: generation
            .zip(authorization)
            .map(|(generation, authorization)| {
                GrantBinding::mint(
                    duration.unwrap_or(GrantDuration::Once),
                    id,
                    decided_at,
                    generation,
                    authorization,
                )
            }),
    };
    let resolved = Resolved { request, decision };
    let dest_dir = match outcome {
        Outcome::Approved => approved_dir(),
        Outcome::Denied => denied_dir(),
    };
    let dest = dest_dir.join(format!("{id}.json"));
    let payload = serde_json::to_string_pretty(&resolved).map_err(|e| e.to_string())?;
    write_atomic(&dest, payload.as_bytes())
        .map_err(|e| format!("write {} {id}: {e}", outcome_dir_name(outcome)))?;

    // Best-effort cleanup of the scratch file. If this fails the
    // authoritative copy is already in approved/ or denied/ and the
    // scratch file is harmless.
    let _ = fs::remove_file(&scratch);

    crate::clawd::system_journal::record_approval_decision(&resolved);

    Ok(resolved)
}

fn invalidate_claimed_request(
    id: &str,
    scratch: &Path,
    request: &Request,
    reason: &str,
) -> Result<(), String> {
    let resolved = Resolved {
        request: request.clone(),
        decision: Decision {
            outcome: Outcome::Denied,
            decided_at: now_secs(),
            decided_by: Some("system:validation".to_string()),
            duration: None,
            note: Some(reason.to_string()),
            grant: None,
        },
    };
    let dest = denied_dir().join(format!("{id}.json"));
    let payload = serde_json::to_string_pretty(&resolved).map_err(|error| error.to_string())?;
    write_atomic(&dest, payload.as_bytes())
        .map_err(|error| format!("invalidate approval {id}: {error}"))?;
    let _ = fs::remove_file(scratch);
    crate::clawd::system_journal::record_approval_decision(&resolved);
    Ok(())
}

fn outcome_dir_name(o: Outcome) -> &'static str {
    match o {
        Outcome::Approved => "approved",
        Outcome::Denied => "denied",
    }
}

// ---------------------------------------------------------------------------
// Public API — read side
// ---------------------------------------------------------------------------

pub fn list_pending() -> Vec<Request> {
    list_pending_for_owner(None)
}

pub fn list_pending_for_owner(owner_uid: Option<u32>) -> Vec<Request> {
    list_dir(&pending_dir())
        .into_iter()
        .filter_map(|p| {
            let data = fs::read_to_string(&p).ok()?;
            serde_json::from_str::<Request>(&data).ok()
        })
        .filter(|request| request_visible_to(request, owner_uid))
        .collect()
}

pub fn list_recent(limit: usize) -> Vec<Resolved> {
    list_recent_for_owner(limit, None)
}

pub fn list_recent_for_owner(limit: usize, owner_uid: Option<u32>) -> Vec<Resolved> {
    let mut out = Vec::new();
    for dir in [approved_dir(), consumed_dir(), denied_dir()] {
        for p in list_dir(&dir) {
            if let Ok(data) = fs::read_to_string(&p) {
                if let Ok(r) = serde_json::from_str::<Resolved>(&data) {
                    if request_visible_to(&r.request, owner_uid) {
                        out.push(r);
                    }
                }
            }
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.decision.decided_at));
    out.truncate(limit);
    out
}

pub fn lookup_pending(id: &str) -> Option<Request> {
    validate_approval_id(id).ok()?;
    let p = pending_dir().join(format!("{id}.json"));
    let data = fs::read_to_string(&p).ok()?;
    serde_json::from_str(&data).ok()
}

/// Atomically retire pending requests created by one task/worker lease.
///
/// A concurrent human decision and teardown race on the same rename;
/// exactly one wins. If the decision wins, the generation revocation
/// performed by the caller still makes its grant unusable.
pub fn invalidate_pending_for_execution(
    owner_uid: Option<u32>,
    session: &str,
    execution: &ApprovalExecutionIdentity,
    reason: &str,
) -> Result<usize, String> {
    ensure_dirs().map_err(|error| format!("approvals dir: {error}"))?;
    let mut invalidated = 0usize;
    for path in list_dir(&pending_dir()) {
        let Ok(data) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(request) = serde_json::from_str::<Request>(&data) else {
            continue;
        };
        let expected_file = format!("{}.json", request.id);
        if validate_approval_id(&request.id).is_err()
            || path.file_name().and_then(|name| name.to_str()) != Some(expected_file.as_str())
        {
            continue;
        }
        if request.owner_uid != owner_uid
            || request.session != session
            || request.execution.as_ref().map(|binding| &binding.identity) != Some(execution)
        {
            continue;
        }
        let scratch = scratch_dir().join(format!("{}.{}.json", request.id, short_id()));
        match fs::rename(&path, &scratch) {
            Ok(()) => {
                invalidate_claimed_request(&request.id, &scratch, &request, reason)?;
                invalidated = invalidated.saturating_add(1);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("claim stale approval {}: {error}", request.id));
            }
        }
    }
    Ok(invalidated)
}

/// Permanently retire all consent state for one in-process Agent invocation.
///
/// The revocation marker is committed before files are moved, so a concurrent
/// decision or a later restore of an old approved record still fails closed.
fn invalidate_local_execution(
    execution: &ApprovalExecutionIdentity,
    reason: &str,
) -> Result<(), String> {
    revoke_execution(execution)?;
    ensure_dirs().map_err(|error| format!("approvals dir: {error}"))?;

    for path in list_dir(&pending_dir()) {
        let Ok(data) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(request) = serde_json::from_str::<Request>(&data) else {
            continue;
        };
        if request.execution.as_ref().map(|binding| &binding.identity) != Some(execution) {
            continue;
        }
        let scratch = scratch_dir().join(format!("{}.{}.json", request.id, short_id()));
        match fs::rename(&path, &scratch) {
            Ok(()) => invalidate_claimed_request(&request.id, &scratch, &request, reason)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "claim ended-invocation approval {}: {error}",
                    request.id
                ));
            }
        }
    }

    crate::filelock::with_exclusive_path_lock(&grant_lock_path(), || {
        for path in list_dir(&approved_dir()) {
            let Ok(data) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(resolved) = serde_json::from_str::<Resolved>(&data) else {
                continue;
            };
            if resolved
                .request
                .execution
                .as_ref()
                .map(|binding| &binding.identity)
                == Some(execution)
            {
                consume_grant_file(&path)?;
            }
        }
        Ok(())
    })
}

fn request_visible_to(request: &Request, owner_uid: Option<u32>) -> bool {
    match owner_uid {
        None => true,
        Some(uid) => request.owner_uid == Some(uid),
    }
}

/// Return an approved grant for `session`/`verb`/`scope`, consuming it
/// atomically when it was approved for `once`.
pub fn consume_matching_grant(
    session: &str,
    verb: Verb,
    requested_scope: &Scope,
) -> Result<Option<GrantDuration>, String> {
    consume_matching_grant_for_owner(session, verb, requested_scope, None)
}

pub fn consume_matching_grant_for_owner(
    session: &str,
    verb: Verb,
    requested_scope: &Scope,
    owner_uid: Option<u32>,
) -> Result<Option<GrantDuration>, String> {
    redeem_matching_grant_for_owner(session, verb, requested_scope, owner_uid, None)
        .map(|grant| grant.map(|grant| grant.duration))
}

/// Atomically spend one Agent approval for an exact capability.
///
/// The returned record is consent evidence, not execution authority.
/// The `agentd` broker must redeem it into a process-bound
/// `clawd::authority` grant before the worker may proceed.
pub fn redeem_matching_grant_for_owner(
    session: &str,
    verb: Verb,
    requested_scope: &Scope,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
) -> Result<Option<ConsumedGrant>, String> {
    let execution = match context {
        Some(_) => Some(local_execution_identity()?),
        None => None,
    };
    redeem_matching_grant_for_execution(
        session,
        verb,
        requested_scope,
        owner_uid,
        context,
        execution.as_ref(),
    )
}

pub fn redeem_matching_grant_for_execution(
    session: &str,
    verb: Verb,
    requested_scope: &Scope,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
    execution: Option<&ApprovalExecutionIdentity>,
) -> Result<Option<ConsumedGrant>, String> {
    let (capability, risk) = canonical_capability(verb, requested_scope.clone())?;
    let expected = ApprovalAuthorization {
        owner_uid,
        session: session.to_string(),
        capability,
        risk,
        context,
        execution: None,
    };
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    // The scan, the budget decrement and the retirement all happen
    // under one store-wide lock, so two callers cannot both spend the
    // last use of the same grant.
    crate::filelock::with_exclusive_path_lock(&grant_lock_path(), || {
        for path in list_dir(&approved_dir()) {
            let Some(resolved) = load_matching_grant(&path, &expected, execution) else {
                continue;
            };
            if let Some(grant) = spend_grant(&path, resolved)? {
                return Ok(Some(grant));
            }
        }
        Ok(None)
    })
}

pub fn redeem_matching_worker_grant_for_owner(
    session: &str,
    verb: Verb,
    requested_scope: &Scope,
    owner_uid: u32,
    execution: &ApprovalExecutionIdentity,
) -> Result<Option<ConsumedGrant>, String> {
    redeem_matching_grant_for_execution(
        session,
        verb,
        requested_scope,
        Some(owner_uid),
        Some(ConsentContext::Attended),
        Some(execution),
    )
}

/// Spend one use of an approved grant, retiring it when the budget runs
/// out.
///
/// Called with the store lock held. Returns `Ok(None)` when another
/// caller won the race and the record is already gone.
fn spend_grant(path: &Path, mut resolved: Resolved) -> Result<Option<ConsumedGrant>, String> {
    let duration = resolved.decision.duration.unwrap_or(GrantDuration::Once);
    let Some(binding) = resolved.decision.grant.as_mut() else {
        // Refused by `load_matching_grant`; belt and braces.
        return Ok(None);
    };
    let Some(generation) = binding.generation else {
        return Ok(None);
    };
    let Some(authorization) = binding.authorization.clone() else {
        return Ok(None);
    };
    binding.uses_remaining = binding.uses_remaining.saturating_sub(1);
    let consumed = ConsumedGrant {
        duration,
        expires_at: binding.expires_at,
        uses_remaining: binding.uses_remaining,
        generation,
        reference: binding.reference.clone(),
        authorization,
    };
    if binding.uses_remaining == 0 {
        return consume_grant_file(path).map(|moved| moved.then_some(consumed));
    }
    let payload = serde_json::to_string_pretty(&resolved).map_err(|e| e.to_string())?;
    write_atomic(path, payload.as_bytes())
        .map_err(|e| format!("spend approved grant {}: {e}", path.display()))?;
    Ok(Some(consumed))
}

/// True when an approved, unconsumed grant already covers this exact
/// capability for this session and owner. Non-consuming: used when
/// re-filing a launch's approval requests so a decision the user has
/// already made is not asked for twice.
pub fn has_approved_grant_for_owner(
    session: &str,
    cap: &Cap,
    owner_uid: Option<u32>,
) -> Result<bool, String> {
    has_approved_grant_for_context(session, cap, owner_uid, None)
}

pub fn has_approved_grant_for_context(
    session: &str,
    cap: &Cap,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
) -> Result<bool, String> {
    let execution = match context {
        Some(_) => Some(local_execution_identity()?),
        None => None,
    };
    has_approved_grant_for_execution(session, cap, owner_uid, context, execution.as_ref())
}

pub fn has_approved_grant_for_execution(
    session: &str,
    cap: &Cap,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
    execution: Option<&ApprovalExecutionIdentity>,
) -> Result<bool, String> {
    let (capability, risk) = canonical_capability(cap.verb, cap.scope.clone())?;
    let expected = ApprovalAuthorization {
        owner_uid,
        session: session.to_string(),
        capability,
        risk,
        context,
        execution: None,
    };
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    Ok(list_dir(&approved_dir())
        .into_iter()
        .any(|path| load_matching_grant(&path, &expected, execution).is_some()))
}

/// Find a pending request for exactly this owner/session/capability and
/// execution context. Scope containment is intentionally not used:
/// consent for one canonical operation must never be substituted for
/// a sibling or broader resource.
pub fn find_pending_exact(
    session: &str,
    cap: &Cap,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
) -> Option<Request> {
    let execution = match context {
        Some(_) => Some(local_execution_identity().ok()?),
        None => None,
    };
    find_pending_exact_for_execution(session, cap, owner_uid, context, execution.as_ref())
}

pub fn find_pending_exact_for_execution(
    session: &str,
    cap: &Cap,
    owner_uid: Option<u32>,
    context: Option<ConsentContext>,
    execution: Option<&ApprovalExecutionIdentity>,
) -> Option<Request> {
    let (cap, risk) = canonical_capability(cap.verb, cap.scope.clone()).ok()?;
    list_pending_for_owner(owner_uid)
        .into_iter()
        .find(|request| {
            request.session == session
                && request.owner_uid == owner_uid
                && request.verb == cap.verb.as_str()
                && request.scope == cap.scope
                && request.context == context
                && request.risk == Some(risk)
                && execution_matches(request.execution.as_ref(), execution, owner_uid, session)
        })
}

pub fn find_pending_worker_request(
    session: &str,
    cap: &Cap,
    owner_uid: u32,
    execution: &ApprovalExecutionIdentity,
) -> Option<Request> {
    find_pending_exact_for_execution(
        session,
        cap,
        Some(owner_uid),
        Some(ConsentContext::Attended),
        Some(execution),
    )
}

fn execution_matches(
    binding: Option<&ApprovalExecutionBinding>,
    expected: Option<&ApprovalExecutionIdentity>,
    owner_uid: Option<u32>,
    session: &str,
) -> bool {
    match (binding, expected) {
        (Some(binding), Some(expected)) => {
            binding.is_live_for(expected, owner_uid, session, now_secs())
        }
        (None, None) => true,
        _ => false,
    }
}

/// Decision state of one request, as reported to the requester.
///
/// Carries no payload beyond the state itself: a requester learns
/// whether it may proceed, never anything about another launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Approved,
    /// Approved earlier and already spent.
    Consumed,
    Denied,
    /// No such request is visible to this owner.
    Unknown,
}

impl RequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestStatus::Pending => "pending",
            RequestStatus::Approved => "approved",
            RequestStatus::Consumed => "consumed",
            RequestStatus::Denied => "denied",
            RequestStatus::Unknown => "unknown",
        }
    }
}

/// Report where `id` currently sits, scoped to `owner_uid`.
pub fn status_for_owner(id: &str, owner_uid: Option<u32>) -> RequestStatus {
    if validate_approval_id(id).is_err() {
        return RequestStatus::Unknown;
    }
    let file = format!("{id}.json");
    for (dir, status) in [
        (pending_dir(), RequestStatus::Pending),
        (approved_dir(), RequestStatus::Approved),
        (consumed_dir(), RequestStatus::Consumed),
        (denied_dir(), RequestStatus::Denied),
    ] {
        let Ok(data) = fs::read_to_string(dir.join(&file)) else {
            continue;
        };
        let visible = match status {
            RequestStatus::Pending => serde_json::from_str::<Request>(&data)
                .map(|request| request_visible_to(&request, owner_uid))
                .unwrap_or(false),
            _ => serde_json::from_str::<Resolved>(&data)
                .map(|resolved| request_visible_to(&resolved.request, owner_uid))
                .unwrap_or(false),
        };
        if visible {
            return status;
        }
    }
    RequestStatus::Unknown
}

/// Retire a whole set of approved grants for one action, all or none.
///
/// Every requested capability must have its own approved grant bound to
/// this session and owner. If even one is missing nothing is consumed,
/// so a launcher can never burn part of an approval set and leave the
/// user re-approving the remainder forever. The scan and the moves run
/// under one store-wide lock, and a failure part-way rolls the already
/// retired grants back.
///
/// Duration is deliberately ignored: `session`/`forever` grants exist so
/// a user-facing session stops being re-prompted, and must not become
/// reusable ambient authority on a path that mints capability-bearing
/// App sessions.
pub fn consume_grant_set_once_for_owner(
    session: &str,
    required: &[Cap],
    owner_uid: Option<u32>,
) -> Result<bool, String> {
    if required.is_empty() {
        return Ok(true);
    }
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    crate::filelock::with_exclusive_path_lock(&grant_lock_path(), || {
        let mut claimed: Vec<PathBuf> = Vec::new();
        for cap in required {
            let (capability, risk) = canonical_capability(cap.verb, cap.scope.clone())?;
            let expected = ApprovalAuthorization {
                owner_uid,
                session: session.to_string(),
                capability,
                risk,
                context: None,
                execution: None,
            };
            let found = list_dir(&approved_dir()).into_iter().find(|path| {
                !claimed.contains(path) && load_matching_grant(path, &expected, None).is_some()
            });
            match found {
                Some(path) => claimed.push(path),
                // Nothing has moved yet, so there is nothing to undo.
                None => return Ok(false),
            }
        }

        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        for path in claimed {
            let Some(file_name) = path.file_name().map(ToOwned::to_owned) else {
                rollback_consumed(&moved);
                return Err("approved grant has no file name".to_string());
            };
            let dest = consumed_dir().join(&file_name);
            match fs::rename(&path, &dest) {
                Ok(()) => moved.push((path, dest)),
                Err(err) => {
                    rollback_consumed(&moved);
                    return Err(format!("consume approved grant {}: {err}", path.display()));
                }
            }
        }
        sync_dir(&approved_dir());
        sync_dir(&consumed_dir());
        Ok(true)
    })
}

fn rollback_consumed(moved: &[(PathBuf, PathBuf)]) {
    for (original, dest) in moved {
        let _ = fs::rename(dest, original);
    }
    sync_dir(&approved_dir());
    sync_dir(&consumed_dir());
}

fn grant_lock_path() -> PathBuf {
    root().join("grants")
}

/// Load `path` when it holds an approved grant for this exact
/// owner, session, verb, scope, risk, and consent context.
///
/// A record with no [`GrantBinding`] is refused. Those are the records
/// written before approvals carried an expiry, a use budget and a
/// revocation generation: the decision they describe was real, but the
/// file alone is not authority, and re-arming one would turn a
/// historical "yes" into standing permission the user was never asked
/// for. An expired, spent or revoked binding is refused the same way,
/// and so is one the revocation state cannot be read for.
fn load_matching_grant(
    path: &Path,
    expected: &ApprovalAuthorization,
    expected_execution: Option<&ApprovalExecutionIdentity>,
) -> Option<Resolved> {
    let data = fs::read_to_string(path).ok()?;
    let resolved = serde_json::from_str::<Resolved>(&data).ok()?;
    if resolved.request.session != expected.session {
        return None;
    }
    if resolved.request.owner_uid != expected.owner_uid {
        return None;
    }
    if Verb::parse(&resolved.request.verb) != Some(expected.capability.verb) {
        return None;
    }
    if resolved.decision.outcome != Outcome::Approved {
        return None;
    }
    if resolved.request.scope != expected.capability.scope {
        return None;
    }
    if resolved.request.context != expected.context {
        return None;
    }
    if resolved.request.risk != Some(expected.risk) {
        return None;
    }
    let grant = resolved.decision.grant.as_ref()?;
    if grant.authorization.as_ref()?.execution != resolved.request.execution {
        return None;
    }
    if !grant.is_live(now_secs(), expected, expected_execution) {
        return None;
    }
    Some(resolved)
}

/// Atomically retire an approved grant. `Ok(false)` means another
/// caller won the race and consumed it first.
fn consume_grant_file(path: &Path) -> Result<bool, String> {
    let Some(file_name) = path.file_name() else {
        return Ok(false);
    };
    let dest = consumed_dir().join(file_name);
    match fs::rename(path, &dest) {
        Ok(()) => {
            sync_dir(&approved_dir());
            sync_dir(&consumed_dir());
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("consume approved grant {}: {err}", path.display())),
    }
}

fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut paths: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|s| s == "json").unwrap_or(false))
        .collect();
    paths.sort();
    paths
}

/// Block-polling waiter. Used by requesters who want a synchronous
/// answer. Polls every 200 ms, gives up after `timeout`.
pub fn wait(id: &str, timeout: Duration) -> Result<Decision, String> {
    validate_approval_id(id)?;
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(200);
    loop {
        let approved = approved_dir().join(format!("{id}.json"));
        let denied = denied_dir().join(format!("{id}.json"));
        if approved.exists() {
            let data = fs::read_to_string(&approved).map_err(|e| e.to_string())?;
            let r: Resolved = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            return Ok(r.decision);
        }
        if denied.exists() {
            let data = fs::read_to_string(&denied).map_err(|e| e.to_string())?;
            let r: Resolved = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            return Ok(r.decision);
        }
        if Instant::now() >= deadline {
            return Err("approval request timed out".into());
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/approvals.rs"
    ));
}

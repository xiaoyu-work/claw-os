//! Broker-side integration with the session event journal.
//!
//! This layer exists so `server.rs` keeps its one job — the order in
//! which a request is admitted — while the rules about *when* a durable
//! mutation may run live next to the journal they depend on.
//!
//! The contract is narrow and fail-closed:
//!
//! 1. [`begin`] runs after authorization and before dispatch. For a
//!    [`Kind::Mutation`] route it appends and fsyncs a
//!    `MutationStarted` record. If that append or its head commit
//!    fails, the request is refused and the handler never runs, so
//!    there is no privileged effect the chain does not know about.
//! 2. [`finish`] runs after the handler returns, once the effect's
//!    durable result is known, and appends `MutationCommitted` or
//!    `MutationFailed`.
//! 3. If *that* append fails, the effect already happened and the
//!    journal cannot say what it did. The response is replaced with an
//!    explicit indeterminate error — never an ordinary success — and an
//!    alarm is raised.
//!
//! The journal lock is never held across the handler: [`begin`] takes
//! it, commits, and releases; the mutation runs; [`finish`] takes it
//! again. The bracket is correlated by an operation id the broker
//! minted, and by the same request/idempotency key the duplicate
//! detector uses — never by a grant handle.

use serde_json::Value;

use crate::session::journal::{
    self, EventSource, JournalError, JournalEvent, MutationBracket, MutationStart, Partition,
    Resolution,
};
use crate::session::SessionId;

use super::authority::Decision;
use super::client_identity::ClientIdentity;
use super::protocol::Response;
use super::routes::{Command, Kind, Route};
use super::wire::{Fault, RequestId};

/// An open bracket plus what the broker needs to close it.
#[must_use = "a bracketed mutation must be closed through finish()"]
pub struct MutationGuard {
    bracket: MutationBracket,
    command: &'static str,
    owner_uid: u32,
}

impl std::fmt::Debug for MutationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutationGuard")
            .field("command", &self.command)
            .field("owner_uid", &self.owner_uid)
            .field("operation", &self.bracket.operation().as_str())
            .finish()
    }
}

impl MutationGuard {
    /// Reference other durable sinks carry instead of a second copy of
    /// the record.
    pub fn reference(&self) -> (String, String, u64) {
        (
            self.bracket.partition().key(),
            self.bracket.operation().as_str().to_string(),
            self.bracket.start_seq(),
        )
    }
}

/// Which chain a route's evidence belongs to.
///
/// A route that resolved a session grant writes into that session's
/// chain; anything else writes into the calling owner's. Both are
/// derived from the authorization decision or the kernel-supplied
/// credentials, never from request parameters.
pub fn partition_for(decision: Option<&Decision>, client: &ClientIdentity) -> (Partition, u32) {
    let owner_uid = decision
        .map(Decision::owner_uid)
        .or(client.uid)
        .unwrap_or(u32::MAX);
    let session = decision
        .and_then(Decision::session_id)
        .and_then(|id| id.parse::<SessionId>().ok());
    match session {
        Some(sid) => (Partition::Session(sid), owner_uid),
        None => (Partition::Owner(owner_uid), owner_uid),
    }
}

/// Open the bracket for a mutation route.
///
/// `Ok(None)` means the route is a query and needs no bracket. `Err`
/// means the request must be refused before dispatch.
pub fn begin(
    route: &'static Route,
    id: &RequestId,
    decision: Option<&Decision>,
    client: &ClientIdentity,
) -> Result<Option<MutationGuard>, Fault> {
    if route.kind != Kind::Mutation {
        return Ok(None);
    }
    let (partition, owner_uid) = partition_for(decision, client);
    let request_key = request_key(owner_uid, id);

    // A request that replays a bracket whose outcome is still unknown is
    // refused rather than re-run: the effect may have landed, and
    // `system.package.install` is not idempotent. The refusal survives
    // restarts and is only lifted by an operator's typed resolution.
    if journal::replays_unresolved(&partition, route.name, &request_key) {
        tracing::error!(
            route = route.name,
            partition = %partition,
            "refusing a request that replays an unresolved mutation"
        );
        return Err(Fault::DuplicateRequest);
    }

    let start = MutationStart {
        partition,
        owner_uid,
        route: route.name,
        request_key: &request_key,
        grant: decision.map(|decision| decision.grant_ref().as_str()),
        session_mutation: None,
        context_ingest: route.command == Command::ContextEventAppend,
    };
    match journal::begin_mutation(start) {
        Ok(bracket) => Ok(Some(MutationGuard {
            bracket,
            command: route.name,
            owner_uid,
        })),
        Err(error) => {
            tracing::error!(
                route = route.name,
                class = error.class(),
                "refusing a mutation the journal could not record"
            );
            journal::alarm::raise(
                journal::alarm::Class::AppendFailed,
                route.name,
                &format!("{} was refused before dispatch: {error}", route.name),
            );
            Err(fault_for(&error))
        }
    }
}

/// Close the bracket once the effect's durable result is known.
///
/// Returns a replacement response when the completion could not be
/// recorded. The caller must send that instead of the handler's answer.
pub fn finish(guard: MutationGuard, id: &RequestId, response: &Response) -> Option<Response> {
    let (partition, operation, start_seq) = guard.reference();
    let command = guard.command;
    let owner_uid = guard.owner_uid;
    let outcome = if response.ok {
        guard.bracket.commit()
    } else {
        let (class, message) = failure_facts(response);
        guard.bracket.fail(class, &message)
    };
    match outcome {
        Ok(_) => {
            super::system_journal::record_journal_mutation(
                command,
                &partition,
                &operation,
                start_seq,
                if response.ok { "committed" } else { "failed" },
                owner_uid,
            );
            None
        }
        Err(unresolved) => {
            super::system_journal::record_journal_mutation(
                command,
                &partition,
                &operation,
                start_seq,
                "indeterminate",
                owner_uid,
            );
            Some(Response::indeterminate(
                id.clone(),
                "mutation_indeterminate",
                unresolved.detail,
            ))
        }
    }
}

/// A failure the journal may record: the route's own stable class, and
/// a message only its keyed digest survives.
fn failure_facts(response: &Response) -> (&'static str, String) {
    match response.error.as_ref() {
        Some(error) => (
            error
                .audit_class
                .unwrap_or(crate::audit_policy::UNCLASSIFIED),
            error.message.clone(),
        ),
        None => (crate::audit_policy::UNCLASSIFIED, String::new()),
    }
}

/// The caller's stable operation key.
///
/// Deliberately **not** [`super::transport::mutation_key`]: that mixes
/// in pid and process start time so a replayed frame from the *same*
/// live process is caught, which is the right key for the in-memory
/// duplicate detector and the wrong one for durable identity. A client
/// that dies mid-mutation and restarts has a new pid, and its retry has
/// to be recognised as the same operation — otherwise the one case
/// where the effect is unknown is exactly the case that gets re-run.
///
/// What is left is the authenticated owner plus the correlation id the
/// caller chose. A caller that wants replay protection across a restart
/// reuses that id; the journal adds the route and keys the whole thing
/// under its root-only key. It selects nothing and authorizes nothing.
fn request_key(owner_uid: u32, id: &RequestId) -> String {
    format!("{owner_uid}|{}", id.as_str())
}

fn fault_for(error: &JournalError) -> Fault {
    match error {
        JournalError::Forbidden { .. } => Fault::NotAuthorized,
        _ => Fault::JournalUnavailable,
    }
}

// ---------------------------------------------------------------------------
// Privileged events other subsystems emit
// ---------------------------------------------------------------------------

/// Record one permission mediation the broker performed.
///
/// Capability issuance and use are journalled by
/// [`super::authority::audit`], which already holds the decision; this
/// is the seam for the approvals path, where the caller knows the
/// session and the owner but not a `Decision`.
pub fn record_approval(partition: &Partition, owner_uid: u32, event: JournalEvent) {
    journal::record_best_effort(partition, owner_uid, EventSource::Kernel, event);
}

// ---------------------------------------------------------------------------
// Operator surface
// ---------------------------------------------------------------------------

/// The single answer a caller gets for a session partition it may not
/// read: one that does not exist, one owned by somebody else, one whose
/// ownership cannot be established, and one whose id is not even
/// well-formed all produce exactly this.
///
/// Distinguishing them would turn this route into an oracle for which
/// session ids exist and which accounts hold them.
const SESSION_UNAVAILABLE: &str = "no session journal partition is available to this caller";

/// Resolve the session the caller named, refusing anything it does not
/// own.
///
/// Ownership is derived from the root-owned session record, never from
/// the request: the body can name a session but cannot select an owner.
/// [`SessionMeta::owner_uid`](crate::session::SessionMeta) is believed
/// only when the record carrying it is root-authored, or when the
/// record's own filesystem owner is the account the field names — any
/// other combination means a third party wrote the claim.
///
/// The check runs *before* any journal read, so a partition the caller
/// may not see is never opened, never verified, never alarmed on and
/// never quarantined. There is no timing or health signal to learn from
/// naming somebody else's session.
fn authorized_session(raw: &str, caller_uid: u32) -> Result<Partition, String> {
    let refuse = || SESSION_UNAVAILABLE.to_string();
    // Syntax first: this is the only entry point that turns an
    // untrusted string into a `SessionId`, and it refuses traversal,
    // whitespace and anything outside the canonical shape.
    let sid = raw.parse::<SessionId>().map_err(|_| refuse())?;

    let record_uid = crate::session::record_owner_uid(&sid).ok_or_else(refuse)?;
    let meta = crate::session::get_meta(&sid).map_err(|_| refuse())?;
    let claimed = meta.owner_uid.ok_or_else(refuse)?;

    if claimed != caller_uid {
        return Err(refuse());
    }
    if record_uid != 0 && record_uid != claimed {
        return Err(refuse());
    }
    Ok(Partition::Session(sid))
}

/// What the journal has to say about the calling owner's own work.
///
/// Read-only, and scoped by the uid the kernel stamped on the message.
/// With no `session_id` the caller gets its own owner partition, which
/// is bound to that uid by construction; with one, the session must be
/// owned by the same uid. Root is not special here — reading another
/// account's session evidence would be an administrative act and would
/// need a route of its own.
///
/// Damage on a partition the caller *does* own is reported rather than
/// hidden, so diagnostics stay available on a chain that mutations are
/// already failing closed on.
pub fn status(params: &serde_json::Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let partition = match params.get("session_id").and_then(Value::as_str) {
        Some(raw) => authorized_session(raw, uid)?,
        None => Partition::Owner(uid),
    };
    let unresolved = journal::unresolved_mutations(&partition, uid).map_err(|error| {
        format!(
            "failed to read the session journal for {partition}: {}",
            error.class()
        )
    })?;
    let projection = journal::projection::build(&partition, uid)
        .map_err(|error| format!("failed to project {partition}: {}", error.class()))?;
    Ok(serde_json::json!({
        "schema": 1,
        "partition": partition.key(),
        "health": projection.health,
        "head_seq": projection.head_seq,
        "quarantined": journal::recovery::quarantined(),
        "unresolved": unresolved,
    }))
}

/// Record an operator's conclusion about an unresolved mutation.
///
/// Root-only by its route descriptor, and bound to the exact partition
/// and operation it names. It re-runs nothing and rolls back nothing:
/// what it records is that a human verified an outcome, which is the
/// only thing that lets the replay refusal end.
pub fn resolve(params: &serde_json::Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    if uid != 0 {
        return Err("resolving a mutation requires root".to_string());
    }
    let key = params
        .get("partition")
        .and_then(Value::as_str)
        .ok_or("partition is required")?;
    let partition = Partition::parse(key).ok_or("partition is not a journal partition")?;
    let operation = params
        .get("operation")
        .and_then(Value::as_str)
        .ok_or("operation is required")?;
    let outcome = match params.get("outcome").and_then(Value::as_str) {
        Some("abandoned") => Resolution::Abandoned,
        Some("committed") => Resolution::Committed,
        Some("rolled-back") => Resolution::RolledBack,
        _ => return Err("outcome must be abandoned, committed or rolled-back".to_string()),
    };
    let owner_uid = match &partition {
        Partition::Owner(owner) => *owner,
        Partition::Session(_) => 0,
    };
    let appended = journal::resolve_mutation(&partition, owner_uid, operation, outcome, uid)
        .map_err(|error| error.class().to_string())?;
    Ok(serde_json::json!({
        "schema": 1,
        "partition": partition.key(),
        "operation": crate::audit_policy::safe_identity(operation),
        "seq": appended.seq,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/journal.rs"
    ));
}

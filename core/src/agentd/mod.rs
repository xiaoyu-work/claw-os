//! `agentd` — the out-of-process agent runtime.
//!
//! Provider HTTP clients, streaming parsers, prompt assembly and tool
//! orchestration run in a short-lived `claw-agentd` process owned by the
//! task's submitter. Dynamic App and MCP code runs in the separate,
//! task-owned [`crate::extension_host`] process; neither executes inside
//! root `clawd`.
//!
//! ## Processes
//!
//! * **`clawd`** stays the authority. It owns the task queue, the
//!   ownership/lease record, session capability derivation, the audit
//!   log and every privileged primitive. It supervises workers but does
//!   not run the live model/tool loop in its own address space —
//!   [`guard`] turns each of those surfaces into a hard error inside
//!   the broker.
//! * **`claw-agentd`** is spawned per task by [`supervisor`]. It starts
//!   as root only long enough to `exec`; [`spawn`] clears supplementary
//!   groups, drops gid then uid to the task owner, sets
//!   `PR_SET_NO_NEW_PRIVS`, applies a `0077` umask, replaces the
//!   environment with an allowlist and closes every inherited
//!   descriptor except the job channel.
//! * **`claw-extension-host`** runs dynamic processes under a reserved uid.
//!   Legacy instances are worker-bound; MCP-first Apps use a daemon-controlled
//!   per-owner instance managed by [`crate::clawd::app_host`].
//!
//! ## Authority
//!
//! The worker receives exactly one narrow authority: a [`grant`] bound
//! to the owner uid, the worker's pid and kernel start time, the task
//! and session ids, a lease deadline and an explicit route allowlist.
//! It is signed with a secret that never leaves the broker process, so
//! a grant cannot be minted by a worker, replayed against a different
//! worker, or presented after its lease expires.
//! The signed claims also pin the extension host pid/start-time, control and
//! proxy paths, and a random task lease nonce.
//!
//! The channel itself is a private `socketpair(2)` created before the
//! fork and handed to the child as fd 3. It carries the job lifecycle
//! routes in [`protocol::WORKER_ROUTES`] plus narrow permission and MCP App
//! Gateway routes — there is no admin, App-session, scheduler or
//! permission-decision route on it, and `/run/cos/clawd.sock` stays
//! `0660 root:sudo` with the worker's supplementary groups cleared, so
//! the worker cannot reach the broker socket at all. Even a leaked fd
//! is therefore only ever an authority to report on, and ask consent
//! for, the single task it was minted for.
//!
//! Note what does *not* authenticate that channel. `SO_PEERCRED` is
//! stamped when the socket is created — before the fork — so it reports
//! the broker's own uid and pid and says nothing about the worker.
//! Authority comes from the grant, bound to the pid and kernel
//! start-time the child actually received.
//!
//! ## Permission mediation
//!
//! The consent store is root-owned and the worker has no broker route,
//! so a denied capability check reaches consent through
//! [`crate::caps::approval_gateway`]: the worker names the exact verb
//! and canonical scope it was refused, and nothing else. `clawd` takes
//! owner, session and task from the verified grant, spends an
//! exactly-matching approved grant one-shot or files/dedupes a pending
//! request under that identity, and answers with a bounded id or a
//! refusal. There is no route to decide a request, to name another
//! session or owner, or to obtain a capability.
//!
//! ## Residual same-uid boundary
//!
//! The worker runs as the task owner rather than a dedicated service
//! account. That is deliberate: the agent loop reads the owner's
//! provider credentials, config, consents and conversation memory, and
//! a separate uid would either need those files opened for it (which
//! widens the credential blast radius) or a second copy of every
//! per-user path. The honest consequence is that a compromised worker
//! has the authority of the account that submitted the task — it can
//! read that user's files and, because per-owner agent state under
//! `<data>/users/<uid>/` is owner-writable, rewrite its own memory and
//! budget counters. It cannot reach root, another account, the broker
//! socket, the job queue, the audit log, or any other user's state.
//!
//! A task owned by root is refused outright, at submission and again
//! before a worker could be forked: there is no lesser account to drop
//! to, so running one would put the model back in a root process. See
//! [`spawn::ROOT_OWNER_REFUSAL`].

pub mod grant;
pub mod app_gateway;
pub mod guard;
pub mod protocol;
#[cfg(unix)]
pub mod spawn;
#[cfg(unix)]
pub mod supervisor;
#[cfg(unix)]
pub mod worker;

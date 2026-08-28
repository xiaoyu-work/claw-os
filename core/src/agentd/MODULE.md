# agentd Module

## Purpose

`agentd/` isolates the agent runtime from the privileged broker. Everything the
model can steer — provider HTTP clients, streaming parsers, prompt assembly,
MCP attachment, dynamic App execution and tool orchestration — runs in a
short-lived `claw-agentd` process owned by the task's submitter. Root `clawd`
supervises, but does not execute it.

## Responsibilities

- Refuse the model/tool runtime inside the broker process (`guard`).
- Drop privilege correctly before the worker's `exec` (`spawn`).
- Mint and verify the narrow per-task authority a worker holds (`grant`).
- Define the only channel a worker has, and what may travel on it
  (`protocol`).
- Claim, lease, supervise, reconcile and finish tasks (`supervisor`).
- Run exactly one task and report it back (`worker`).

## Key Files

| Path | Role |
| --- | --- |
| `guard.rs` | Process-wide broker flag; every runtime surface fails closed inside `clawd` |
| `spawn.rs` | `socketpair` + `pre_exec` privilege drop, fd/env isolation, session/process-group isolation, worker image checks |
| `grant.rs` | HMAC-signed job grant, its bindings, and both verification directions |
| `protocol.rs` | Frames, route allowlist, protocol version, bounded framing, permission-mediation types |
| `supervisor.rs` | Broker-side claim → spawn → lease → pump → finish, permission mediation, reconciliation |
| `worker.rs` | Worker-side handshake, dedicated channel thread, sinks, audit forwarding, approval gateway, cancellation |

## Threat Boundary

The worker starts as root only long enough to `exec`. In one `pre_exec`
closure, in this order: `umask(0077)`; `dup2` the job channel to fd 3 and mark
every other descriptor close-on-exec; `setgroups(0, NULL)` **before** the uid
drop (this is what removes `sudo`); `setresgid` then `setresuid`; re-read all
ids and the supplementary group list from the kernel and abort if any is wrong;
`PR_SET_PDEATHSIG` with a `getppid` re-check; `setsid`, so the runtime leads its
own session and process group; `PR_SET_NO_NEW_PRIVS`. The environment is
rebuilt from an allowlist and carries no credential value.

Only the forking thread survives `fork`, so nothing in that closure allocates,
formats, logs or takes a lock: failures are reported as bare `errno` values via
`Error::from_raw_os_error`, which is all `std` writes to its exec-status pipe
anyway.

`/run/cos/clawd.sock` is unchanged (`0660 root:sudo`). Because the worker's
supplementary groups are cleared it cannot open that socket at all, so it has
no broker route: no admin, App-session, scheduler or permission-decision
surface is reachable. Its only authority is the grant, bound to owner uid,
worker pid plus kernel start time, task and session id, a lease deadline and
the routes in `protocol::WORKER_ROUTES`. The signing key never leaves the
broker process, so a grant cannot be minted, edited, replayed against another
worker, or used past its lease. Executable path, TTY, `NoNewPrivs`, socket
group and prompt text confer nothing.

`SO_PEERCRED` is deliberately *not* used on this channel: the socket pair is
created before the fork, so the kernel stamps it with the broker's own uid and
pid. Checking it would prove nothing about the worker.

Capabilities are derived by `clawd` from root-owned session metadata
(`clawd::session_scope`) and handed to the worker; the worker never authors
them, and the exact tool/provider checks still run at the execution boundary
inside it.

## Permission Mediation

`<caps-data>/approvals` is root-owned and the worker has no broker route, so a
denied capability check reaches consent over the job channel rather than the
filesystem. `caps::approval_gateway` is the seam: `caps::require` uses it in
place of the local store whenever one is installed, and only `claw-agentd`
installs one.

- The worker sends the **exact denied verb and canonical scope**, and nothing
  else. There is no session, owner, task, requester, reason, duration or
  capability field on the wire.
- `clawd` takes owner, session and task from the verified grant, re-parses the
  verb against the catalog, rejects a scope that will not render as a bounded
  single-line record, and composes the reason text itself from the catalog
  label.
- `Consume` spends one exactly-matching approved grant, one-shot; a replay
  finds nothing. `Request` files or dedupes a pending request under the
  grant-bound session and owner and returns a bounded id. The worker records
  that id, interrupts its turn, and reports a waiting outcome instead of
  converting the denial into a terminal task error.
- There is no decide route. A worker can never approve anything, name another
  session or owner, or receive a reusable capability.
- Mediation is bounded on both sides (`protocol::MAX_APPROVAL_ASKS`), refused
  once the lease expires or the task is cancelled, and every mediated decision
  is audited by the broker.

Channel I/O runs on its own thread inside the worker. `caps::require` is
synchronous, so the gateway blocks its caller while it waits for the broker's
immediate filing response; keeping the reader off the agent runtime's threads
is what stops that from deadlocking against streaming or tool execution. Human
wait time does not hold a worker: the queue persists the task under `waiting/`,
and the supervisor requeues it after approval.

## Residual Same-UID Boundary

The worker runs as the **task owner**, not a dedicated service account. That is
deliberate: the agent loop reads the owner's provider credentials, config,
consents and conversation memory, and a separate uid would either need those
opened for it — widening the credential blast radius — or a second copy of
every per-user path.

The honest consequence: a compromised worker holds the authority of the account
that submitted the task. Per-owner agent state under `<data>/users/<uid>/`
(conversation memory, notes, todos, AI budget counters, AI run records) is
owner-owned `0700` so the worker can write it, which means that account can also
rewrite its own memory and usage counters — data it already authored or could
already influence through prompts and its own home-directory config. It cannot
reach root, another account, the broker socket, the job queue, the audit log, or
any other user's state. `<data>` and `<data>/users` are `0711`: traversable,
never listable; every other subtree stays `0700 root`.

A task owned by **root is refused**, both at `task.submit` and again before a
worker could be forked, because there is no lesser account to drop to. Running
one would put the model/tool loop back in a root process, which is the thing
this module exists to prevent. Single-account container and WSL images must give
the agent its own unprivileged account; there is no opt-out switch.

## Known Consequences

Removing the worker's broker access is deliberate, and two things change with
it:

- **App and MCP sessions started from inside a task.** Registering one needs
  either an `app_session.*` broker route or a write to the root-owned routed
  capability registry at `/run/cos/caps/<uid>` (`0750 root:<gid>`, read-only to
  the owner precisely so the delegated account cannot forge its own
  capabilities). A worker has neither, so such a launch now fails closed
  instead of running with the broker's authority. Restoring it needs a
  sandboxed App/MCP host, which is tracked separately; widening either surface
  here would put back the authority this change removes.
- **Scheduler mutation from inside a task.** `cos cron` / `cos triggers` state
  lives in the root-owned daemon tree, so a worker can read its own scope but
  cannot persist system schedules. `scheduler.run` is a broker route and is not
  on the worker channel.

Ordinary capability denials are *not* affected: they still reach consent, via
the permission mediation route above.

Audit fidelity is preserved but not identical: the keyed text digests in
forwarded records are computed with the worker's per-process key, so two
records correlate within a task rather than across the daemon's lifetime.

## Failure and Upgrade Behavior

A worker that panics, is killed, exits without a result, stops heartbeating,
sends a frame outside its grant, or speaks a different protocol version only
ends its own task. The supervisor terminates the worker's whole process group —
so no App, MCP server or shell it started survives — reaps it, then either
releases the task for retry (bounded by the same recovery budget as orphan
recovery) or fails it, and `clawd` keeps serving. `clawd` treats no worker exit
— normal or not — as fatal.

Approval waits are a separate durable lifecycle, not crash retries. A waiting
task holds no worker slot or lease, survives daemon restarts, can be cancelled
immediately, and resumes under a fresh worker with the same task and session
identity after all linked approvals are granted. An undecided wait expires
after eight hours so abandoned consent prompts cannot retain tasks forever.

Mixed installs fail closed: both sides check `protocol::PROTOCOL_VERSION` and
report a named mismatch that names the fix. `PR_SET_PDEATHSIG` means a worker
cannot outlive the daemon that leased it, so every task left in `running/` at
start-up belongs to a dead worker and is reconciled before the first claim.
`CLAWD_AGENTD=off` disables supervision only; every other `clawd` primitive
keeps working.

## Configuration

| Variable | Meaning |
| --- | --- |
| `CLAWD_AGENTD` | `off` disables agent supervision (default on) |
| `COS_AGENTD_BIN` | Worker executable (default: beside `clawd`, else `/usr/local/bin/claw-agentd`) |
| `CLAWD_AGENTD_MAX_WORKERS` | Concurrent workers, 1–64 (default 4) |
| `CLAWD_AGENTD_LEASE_SECS` | Heartbeat lease, 30–86400 (default 900) |
| `CLAWD_AGENTD_HEARTBEAT_GRACE_SECS` | Handshake grace, 10–3600 (default 120) |
| `CLAWD_AGENTD_POLL_MS` | Queue poll interval (default 500) |

## Dependencies

- `crate::agent::service` for the task queue, `JobExecution` and `FinishOutcome`.
- `crate::caps::approval_gateway` for the consent seam `caps::require` consults.
- `crate::approvals` and `crate::clawd::{audit, session_scope}` for consent
  mediation, the audit sink and capability derivation.
- `crate::proc` for kernel process identity, `crate::storage` for owner state provisioning.

## Tests

```bash
cargo test -p cos agentd -- --test-threads=1
cargo test -p cos caps::enforcement -- --test-threads=1
cargo test -p cos --test agentd_process_boundary -- --test-threads=1
bash packaging/deb/tests/test-agentd-packaging.sh
```

`core/tests/agentd_process_boundary.rs` spawns a real worker and asserts the
kernel's view of it (`/proc/<pid>/status`, `/proc/<pid>/fd`,
`/proc/<pid>/environ`), and drives `supervisor::run_with_store` end to end —
claim, spawn, handshake, assignment, result, finish — against a temporary
queue, including the root-owner refusal, which must resolve the task without
ever executing a worker image.

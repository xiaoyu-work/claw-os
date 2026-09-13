# Activity execution limits

Execution limits put finite attempt, model-turn and expiry bounds on an
Activity. They are **constraints, never capability grants**. Existing
permissions, approvals, worker isolation and audit remain authoritative.
This is the resource-limiting increment of bounded delegation, not a complete
delegation policy language or token/cost budget.

Terminal, Web and native desktop use one owner-scoped policy in
`activities.db`. An Activity without a configured policy retains its existing
task behavior. Disabling a configured policy blocks its work; it does not
delete the policy or restore unlimited execution.

## Configure a finite policy

In an installed Linux or WSL environment, use an existing Activity ID:

```bash
activity_id="00000000-0000-4000-8000-000000000001"

cos activity set-execution-limits "$activity_id" \
  --max-attempts 10 --max-turns 5 --expires-at 2026-09-18T00:00:00Z

cos activity execution-limits "$activity_id"
```

Choose a future RFC3339 expiry. The backend normalizes it to UTC. Allowed
ranges are 1-1000 lifetime attempts and 1-100 model turns per attempt.
These numbers do not authorize any App operation or privileged effect.

The first policy is enabled at revision 1. Later edits require the current
revision:

```bash
cos activity set-execution-limits "$activity_id" --revision 1 \
  --max-attempts 15 --max-turns 4 --expires-at 2026-09-19T00:00:00Z

cos activity disable-execution-limits "$activity_id" --revision 2
cos activity enable-execution-limits "$activity_id" --revision 3
```

Updates preserve the used-attempt counter and enabled state. They never
silently reset usage or re-enable disabled work. Raising a ceiling is an
explicit user change. Lowering it below already-used attempts is allowed and
blocks new attempts. There is no delete or reset endpoint.

Creating, editing and enabling require an active or paused Activity.
Disabling remains possible after the Activity ends. Re-enabling requires
unexpired settings. A stale revision is an explicit conflict, not an
automatic overwrite.

## When an attempt is charged

The daemon charges an attempt transactionally when claiming an Activity job,
before the filesystem transition into running state. It records a root-created
reservation ID, job/Activity/owner, policy revision, maximum turns and expiry.
The reservation is accounting data, not authority or proof of execution.

The charge is conservative: a subsequent startup, recording or filesystem
failure can consume the attempt. There are no automatic refunds. A recovered
or retried job is a fresh attempt and consumes another reservation. Exact
internal retries of the same current reservation do not double-charge.

An inactive Activity remains subject to its existing queue gate. Disabled,
expired or exhausted policies refuse new attempts visibly without blocking
unrelated queued jobs. A refused queued job can be retried explicitly after
the owner changes the policy. Policy changes do not complete or reopen an
Activity.

## Enforcement during execution

The root supervisor clamps the worker's actual `max_turns` to its reservation,
not merely to text in a prompt. The standalone execution path uses the same
turn ceiling and live policy guard.

At execution start, the guard derives a monotonic deadline from the reserved
expiry. Heartbeats cannot extend it, and moving the wall clock backwards does
not renew the running attempt. Live policy checks also detect disable,
expiry and revision changes. Newly configuring a policy requires previously
unreserved work to start a fresh bounded attempt.

Policy changes close new App-host admission and use the normal cancellation
and exact-child cleanup path. Already-admitted privileged mutations are not
promised to be undone. A budget stop is reported with its own failure reason,
not mislabeled as a user cancellation or successful completion.

Final worker results are checked against the live policy and reserved turn
ceiling. The task stream records reservation and stop metadata. No serialized
reservation, policy record or model response is promoted into a grant.

## Shared controls and persistence

The Web/native **Execution limits** card shows policy state, used and remaining
attempts, per-attempt turns, expiry and revision. Its forms use the same
compare-and-swap contract as the terminal. A client must refresh after a stale
revision rather than silently apply an edit to a newer policy. Counters and
goal state remain backend-owned.

| Broker route | Request |
| --- | --- |
| `activity.execution_limits.get` | Activity ID |
| `activity.execution_limits.set` | Activity ID, expected revision or initial absence, bounded limits |
| `activity.execution_limits.enabled` | Activity ID, expected revision, explicit enabled flag |

Requests cannot supply an owner, used-attempt counter, reservation or
capability set. Root has no cross-owner exemption.

Database schema 4 adds policy and reservation accounting to the same Activity
database, preserving goals, receipts and object-state history. Private file
modes, transactional migration, integrity and ownership checks remain in
force. Unsupported/corrupt state is an error, never a reset or unlimited
fallback. See [updating](updating.md).

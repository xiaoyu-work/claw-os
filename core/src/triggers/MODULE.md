# Trigger Module

## Responsibility

Deterministically match owner-visible context events and submit ordinary Agent
Jobs. Rules retain the scheduler's existing owner, home and capability ceiling.
Activity association adds constraints and correlation, never authority.

## Key files

- [`../triggers.rs`](../triggers.rs): CLI validation, rule persistence, matching,
  owner checks, read-only `preflight_activity_command` broker integration and
  unattended `ScheduledTrigger` Session preparation.
- [`activity.rs`](activity.rs): owner-scoped Activity preflight, bounded delivery
  evidence and rule diagnostics through existing audit/notification services.
- [`delivery.rs`](delivery.rs): shared scheduler file lock, compatible event
  cursor, legacy retries and one durable in-flight Activity delivery.

## Delivery contract

Activity triggers require an active owned Activity, configured enabled finite
execution limits, remaining attempts and future expiry. A configured capability
policy must remain enabled; missing capability policy means ordinary authority.
Root Job claim still charges attempts and enforces actual limits and policies.
Broker preflight shares command validation and UUID canonicalization with the
CLI. It checks owned stored rules for `enable`/`run` before consent, but never
seeds rules, records diagnostics or creates work. A `list --activity` filter is
syntax-only, so paused and terminal Activity rules remain inspectable without
opening the Activity backend. Other commands reject an Activity override.

Before the first Activity rule is published, its existing or initial cursor is
written and synced, followed by a durable `.activity-cursor-initialized` marker.
The marker survives rule removal so replacement cannot reset lost progress.
An absent cursor with this marker or any stored Activity rule returns an
actionable indeterminate-progress error; tick, associated add/run and re-arm
never recreate it. Broker preflight performs the same check without writes.
Listing, disabling and removal remain available; retirement preserves the
marker without inventing a cursor. Restore the original durable cursor to
resume normal consumption or in-flight Job recovery. There is no reset flag.
Legacy-only stores that have never initialized Activity progress retain their
original missing-cursor behavior.

Observed blocked events are consumed, not backlogged for resume. Submission
persists a delivery UUID, then its Session correlation, before publishing a Job
with that UUID through the shared queue. Recovery only recognizes an existing
matching owned Activity Job/Session. Missing or conflicting evidence produces an
`indeterminate` diagnostic and disables the rule, without replay. Explicit
enable rotates the rule generation; removed or replaced rules cannot inherit
older pending deliveries. Re-arming does not cancel already-published Jobs.

`list` includes optional `activity_id`, `generation` and `last_delivery`
(`status`, `at_ms`, optional `job_id`/`session_id`, `message`). `tick` retains
`processed`, `cursor`, `fired`, `pending` and adds `skipped` diagnostics.
The singleton in-flight record is correlation, not a workflow or a grant.
Unassociated rules never open the Activity database.

## Validation

From the repository root, with isolated test state and serial environment tests:

```bash
cargo test -p cos --lib triggers:: -- --test-threads=1
```

Unit bodies mirror these sources under
[`core/test/unit/triggers.rs`](../../test/unit/triggers.rs) and
[`core/test/unit/triggers/`](../../test/unit/triggers).
See the shared [Activity](../../../docs/activities.md) and
[execution-limit](../../../docs/activity-execution-limits.md) contracts.

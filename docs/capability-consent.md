# Capability-aware Agent consent

Agent consent is an extension of the capability system, not a parallel source
of authority. A model-visible tool name never determines whether an operation
may run.

## Decision flow

1. The tool validates its structured arguments.
2. The owning primitive derives the exact capability verb and canonical scope.
3. `caps::require` checks the session's existing capability set.
4. If the capability is missing:
   - an **attended** system-Agent session may file one request for that exact
     verb, scope, catalog risk, owner, session, task, worker pid/start time,
     lease nonce/deadline, request generation, and consent context;
   - an **unattended** cron/trigger session fails closed and must receive the
     exact authority when the automation is created.
5. An approval creates a time-bounded, use-bounded, generation-bound consent
   record. Critical Agent requests are one-shot; high-risk Agent requests may
   be one-shot or session-limited, but not `forever`.
6. When `claw-agentd` retries the operation, `clawd` atomically spends the
   exact consent record and redeems it into an in-memory one-use capability
   grant bound to the owner, session, task, worker pid/start time, verb, scope,
   approval expiry, and revocation generation.
7. The primitive's execution-time `caps::require` call succeeds only after that
   grant is exercised. The capability check still occurs immediately before
   the guarded operation; approval never edits the session's ambient `CapSet`.

Daemon workers take their task identity from the authenticated broker lease.
In-process runtimes (including the multiplexed web server) instead install a
Tokio task-local identity for each Agent invocation. Web identities include the
conversation and a fresh turn ID; every invocation also receives a fresh
nonce. There is no process-wide fallback identity.

For example, `cos_proc status <session>` derives low-risk
`proc.observe:self:<session>`, while `cos_proc kill <session>` derives
high-risk `proc.signal:self:<session>`. They share one proxy tool name but not
one consent decision.

Model output cannot approve a request. The worker channel has no decision
route, and a request id is metadata rather than authority.

## Replay, expiry, and revocation

- Capability and scope matching is exact; scope containment is not used to
  substitute a broader approval for the validated operation.
- A decision or redemption fails if the originating process is gone, the task
  or lease identity differs, the request deadline passed, or the revocation
  generation changed. A replacement worker is never silently rebound.
- Ending or cancelling an in-process invocation writes a durable
  execution-revocation marker and retires its pending and approved records.
  A later turn, concurrent conversation, disconnected client, or restored
  approval file cannot reuse that invocation's consent.
- `once` has one use. `session` and `forever` remain use-limited and expire.
- Concurrent spends and concurrent approve/deny decisions have one winner.
- Revocation increments a root-owned owner/session generation. Every grant
  checks that generation, so restoring an older approval file cannot revive
  retired consent. Owner-wide revocation advances beyond the highest session
  generation for that owner rather than merely incrementing the owner counter.
- Approval records created before exact authorization metadata existed remain
  visible as history but grant nothing.

Audit records correlate the durable approval reference with the task/worker
redemption and the one-use authority-grant reference without logging opaque
handles or secret scope values.

## `dangerous_tools` migration

`dangerous_tools` is retained only as a compatibility prompt for tools that do
not expose a capability-aware execution boundary. Capability-aware core
`cos_*` primitive proxies may contain both read and write commands, so their
tool name is too coarse and no longer intercepts execution.

- Use `auto_deny_tools` for a hard operator block on an entire tool.
- `auto_approve_tools` only bypasses the legacy name prompt. It never grants or
  widens a capability.
- Remove capability-aware core proxy names from `dangerous_tools`; exact
  capability risk and scope now drive their consent requests.
- Keep mixed or incomplete proxies on `dangerous_tools` until every sensitive
  branch has an exact mapping. `cos_sysinfo` remains legacy-filtered, and its
  `env --include-secrets` branch separately requires
  `secret.read:name:environment`; a low-risk `sys.observe` grant is
  insufficient.

The authoritative implementation is in
[`core/src/caps/enforcement.rs`](../core/src/caps/enforcement.rs),
[`core/src/approvals.rs`](../core/src/approvals.rs), and
[`core/src/agentd/supervisor.rs`](../core/src/agentd/supervisor.rs).

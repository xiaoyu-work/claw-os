# Session Journal Module

## Purpose

`session/journal/` is the machine's authoritative, ordered record of
what happened in a session and whether a privileged mutation finished.
It is evidence and recovery state — never authority.

## Responsibilities

- Define a typed, versioned event schema with closed, bounded fields.
- Append events as a MAC-chained, single-writer, fsynced log with a
  separately committed head anchor.
- Bracket every durable mutation: a start that must land before any side
  effect, and a committed/failed record once the outcome is known.
- Keep an unresolved mutation refusing its own replay until an operator
  states what happened.
- Verify chains on startup and resume, flag orphaned brackets, and
  quarantine partitions that do not verify.
- Rebuild the mutation timeline and agent lifecycle views from the chain
  so no consumer keeps a second, drifting copy.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Facade, errors, durable primitives, bracket, identity, resolution |
| `event.rs` | Versioned event schema and bounded reference types |
| `acl.rs` | Which source may record which event kind |
| `record.rs` | Stored record and the length-prefixed MAC preimage |
| `keyring.rs` | Root-only MAC keys, ownership/mode checks, rotation |
| `partition.rs` | Partition identity, segments, and the signed head anchor |
| `writer.rs` | Writer lease/epoch, partition lock, durable append, rotation |
| `reader.rs` | Whole-chain verification across segments |
| `quota.rs` | Capacity classes and the computed closure reserve |
| `recovery.rs` | Startup/resume scan, unresolved set, quarantine |
| `projection.rs` | Rebuildable mutation and lifecycle views |
| `alarm.rs` | Bounded, independent failure channel |
| `../../../test/unit/session/journal/` | Unit and adversarial tests |
| `../../../tests/session_journal_process.rs` | Cross-process tests |

## Dependencies

The journal consumes `crypto`, `storage`, `paths` and `audit_policy`.
`clawd::journal` is the broker's integration layer; `clawd::server`
calls it around dispatch, `agentd::supervisor` forwards the worker's
tool and turn lifecycle through it, and `clawd::authority::audit`
mirrors capability facts into it. Nothing in the journal depends on a
provider or a route handler.

## Security properties

- **Not authority.** Replaying `CapabilityIssued`, `ApprovalDecided` or
  any other event creates no live grant. The capability authority and
  the approvals store keep their own state; the chain holds keyed
  references and outcomes.
- **Single writer.** Appending requires the process-wide writer lease,
  which is an exclusive `flock` on `writer.lock` plus a monotonic epoch
  stamped into every record and the head anchor.
- **Structural ACL.** `EventSource::may_write` matches on the event
  variant, so a worker frame can request tool and turn lifecycle and
  nothing else, and a new event kind does not compile until its row is
  decided.
- **Secret-safe.** No `serde_json::Value`, no free `String`. Text that
  may never be stored becomes a keyed digest; owner-private content
  becomes a content-addressed reference that is not usable as authority.
- **Anti-rollback, stated honestly.** The committed head defeats
  truncation, reordering, injection and stale writers for a local
  unprivileged attacker. Root, or physical access, restoring a
  consistent older snapshot of key + chain + anchor is **outside** this
  threat model; there is no TPM or remote anchor to compare against.

## Load-bearing invariants

### Every committed state is reader-valid

A chain is a sequence of numbered segments under `segments/`; only the
highest is appended to. Rotation is therefore one anchor commit that
names the next index, its first sequence and the MAC it chains from —
no file is moved. Before the commit the old segment is active; after it
the new index is active and its file does not exist yet, which is what
"zero bytes committed" means. There is no crash boundary at which a
reader must decide between truncation and tampering.

The retention record that names the archived segment is owed by a
`pending_retention` marker in the anchor, and the commit that stores the
record clears the marker in the same atomic write, so it is written
exactly once across a crash rather than at least once.

### A missing head is damage, not a fresh partition

`load_anchor` distinguishes "never written" from "the head was deleted"
by asking whether any segment still holds bytes. A partition with chain
bytes and no anchor raises `AnchorMissing`: the bytes are preserved,
mutations fail closed, and recovery quarantines it. Nothing truncates or
adopts it, because unlinking one small file must not be a way to make
the daemon erase a committed chain.

### Only closure records may use the reserve

Capacity is split by *event kind first*: records that retire, flag or
recover a bracket are `Closure`; everything a model, tool or peer can
drive — capability use, approval mediation, prompt snapshots, turns,
mutation starts — is bounded `Control`, `Worker` or `ContextIngest`
traffic even when the broker is the writer. The reserve is computed from
the anchor's open-bracket count rather than guessed, so a start is only
admitted when its own closure is already paid for, and the per-class
ceilings sum to strictly less than the partition ceiling as an
independent second bound.

### An unresolved mutation keeps refusing its replay

Durable operation identity is `owner uid + canonical route + the
caller's operation key`, keyed under the root-only journal key. It
deliberately excludes pid and start time — the transport's duplicate
detector keeps those — because the one case where the effect is unknown
is exactly the case where the client has restarted with a new pid.

`MutationOrphaned` and `MutationIndeterminate` are *flags*: they say the
outcome is unknown. Only `MutationCommitted`, `MutationFailed` or an
operator's `MutationResolved` retires a bracket, so the refusal
survives any number of restarts. `journal.mutation.resolve` is the
root-only route that records that statement; it re-runs nothing and
grants nothing.

### `journal.status` derives ownership, it does not infer it

Naming a session in the request body selects a *lookup*, never an
owner. `journal.status` resolves the session id, reads the owner from
the root-owned session record, and requires it to equal the uid the
kernel stamped on the message — before it opens the partition at all.
`SessionMeta::owner_uid` is believed only when the record carrying it is
root-authored, or when the record's own filesystem owner is the account
the field names; anything else means a third party wrote the claim.

A session that does not exist, one owned by somebody else, one whose
ownership cannot be established and an id that is not well-formed all
return the same bounded refusal, so the route is not an oracle for which
sessions exist or who holds them. Because authorization precedes the
read, a partition the caller may not see is never opened, never
verified, never alarmed on and never quarantined. Root is not special:
reading another account's session evidence would be an administrative
act and would need a route of its own.

### The fault injector cannot exist in a shipped binary

`session/journal/mod.rs` has a fault injector for the two durability
paths, because "a failed start never dispatches" and "a failed
completion is indeterminate" are exactly the branches a real disk
failure takes. The module, the `Fault` enum, the armed state, the
setters and the failure branch are all `#[cfg(test)]`, **and so are both
call sites**, so a non-test build contains no hook, no state and no
branch. There is deliberately no production no-op shim.

Nothing in the journal reads the environment or any configuration, so
there is no runtime channel that could arm a fault even in a test
binary. Source guards in `test/unit/session/journal/mod.rs` assert all
of this — the module's `cfg(test)` attribute, the exact number of gated
call sites, the absence of environment reads and the absence of a public
setter — and they fail if a later change re-introduces any of them.

## Tests

```bash
cargo test -p cos journal -- --test-threads=1
cargo test -p cos --test session_journal_process -- --test-threads=1
```
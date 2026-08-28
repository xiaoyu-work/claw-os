# Improvements

This document tracks outstanding design-level improvements. It contains only
work that remains relevant to the current architecture; completed or
superseded proposals should be removed rather than left as ambiguous roadmap
items.

## Memory Subsystem

### Current state

Claw OS uses four complementary memory stores:

| Store | Purpose | Read path |
| --- | --- | --- |
| `USER.md` | Durable user preferences and persona facts | Bounded prompt-note selection |
| `MEMORY.md` | Curated task and project context | Bounded prompt-note selection |
| `memory.db` | Complete message history with SQLite FTS5 | `cos_recall` |
| `semantic.db` | Model-defined embedding vectors in SQLite | `cos_recall_semantic` |

Prompt assembly no longer injects an unbounded prefix of the two Markdown
files. It selects a bounded note projection while preserving the complete
files on disk. The curator also performs exact duplicate and correction
handling before writing facts.

SQLite memory already exposes explicit count/purge operations. What is still
missing is automated retention policy and fuzzy duplicate handling. Supported
corruption diagnosis, FTS rebuild, and evidence-preserving quarantine are
documented in [`memory-recovery.md`](memory-recovery.md).

### M1 — Semantic deduplication before curator append

**Problem.** Exact deduplication does not merge semantically equivalent facts
such as "user prefers vim" and "the user's editor is vim".

**Proposed fix.**

1. Embed each candidate curator fact.
2. Compare it with existing live facts.
3. Above a documented similarity threshold, merge or replace the existing
   fact instead of appending another line.
4. Preserve the current correction behavior so a newer contradictory fact can
   replace an older one.

**Primary code:** `core/src/agent/memory/curator.rs`

**Acceptance:** repeating one preference in different wording leaves one
current fact, while a genuine correction replaces the old value.

### M2 — `USER.md` deduplication

**Problem.** `cos_memory append USER.md` remains unconditional, so the Agent
can append the same durable preference in separate sessions.

**Proposed fix.** Add default-on deduplication for `USER.md` using a cheap
exact/token-set check, with an explicit override for intentional duplicates.

**Primary code:** `core/src/agent/tools/cos_proxy/memory.rs`

**Acceptance:** two near-identical preference appends leave one line.

### M3 — Configurable raw-store retention

**Problem.** `memory.db` and `semantic.db` have purge primitives but no
configuration or scheduled janitor.

**Proposed fix.**

- Add `agent.memory_ttl_days` and `agent.semantic_ttl_days`.
- Default both to `0` (never purge).
- Run a low-frequency janitor only when a TTL is configured.
- Keep recently curated durable facts in Markdown before raw rows expire.

**Primary code:** memory stores, Agent config, and Agent service startup.

**Acceptance:** configured TTLs remove only expired rows and leave recent
history untouched.

### Superseded memory proposals

The following older proposals are intentionally removed:

- rolling `MEMORY.md` into date-based journal files,
- adding `cos_recall_journal`,
- time-bucket partitioning of the SQLite stores,
- adding HNSW/IVF before current vector volumes justify it.

Bounded prompt-note selection removed the immediate need for a rolling archive.
The journal tool depended on that archive and therefore no longer has a source
of data.

## System Introspection

### Current state

`cos_sysinfo` provides request/response reads for process, network, storage,
logs, systemd, and package state.

`clawd` already owns:

- desktop-control and clipboard providers,
- a persistent event center,
- udev, systemd, journal, storage, security, and pidfd process-exit event
  sources.

New work should extend these existing providers rather than introduce a second
event bus inside the Agent runtime.

### S1 — Expose complete desktop state

**Problem.** `cos_sysinfo desktop` still exposes environment hints rather than
a complete view of active windows, displays, and clipboard availability.

**Proposed fix.**

- Reuse the existing `clawd` desktop and clipboard providers.
- Add read-only window/display snapshot operations.
- Keep clipboard content opt-in and capability-gated.
- Project the resulting state through the Agent's normal tool and audit paths.

**Acceptance:** the Agent can identify active windows and displays without
shelling out to desktop-specific commands, while clipboard content remains
off by default.

### S2 — Bridge `clawd` events into Agent wake-up

**Problem.** `clawd::event_center` collects and persists system events, but the
Agent does not yet have one complete wake/backlog path that turns selected
events into resumable Agent work.

**Proposed fix.**

1. Keep `clawd::event_center` as the single system event collector.
2. Add policy-controlled subscriptions that select which events can wake the
   Agent.
3. Persist the selected event cursor with Agent task/session state.
4. Inject the event through the same traced context path used by interactive
   requests.

**Acceptance:** a configured event can wake the Agent exactly once, survive a
restart, and remain reconstructable from session/audit records.

### S3 — Reversible signal log

**Problem.** Process signal/kill operations do not preserve enough context to
explain or partially recover from a mistaken termination.

**Proposed fix.**

- Record timestamp, PID, signal, command identity, and Agent session before
  sending the signal.
- Expose recent signal history through a read-only system tool.
- Optionally support restarting the previous command when that is safe; do not
  describe this as restoring lost in-memory process state.

**Primary code:** `core/src/proc.rs` plus the system audit/query surface.

**Acceptance:** every Agent-issued signal has a durable breadcrumb, and an
operator can identify what was terminated.

### S4 — Two-sample metrics for remaining counters

`top`, `disk_io`, and `net_rate` already use two samples. Apply the same
pattern where meaningful to:

- per-cgroup CPU usage,
- per-uid network usage,
- per-process I/O rates.

**Acceptance:** rate fields are based on elapsed samples rather than raw
cumulative counters.

## Suggested Order

1. **S2** — complete the existing event-center-to-Agent path.
2. **M1** — reduce long-term memory duplication.
3. **S1** — expose complete desktop state through existing providers.
4. **S3** — add durable signal breadcrumbs.
5. **M2** — deduplicate Agent-authored user facts.
6. **S4** — extend rate sampling.
7. **M3** — add retention automation when real data volume requires it.

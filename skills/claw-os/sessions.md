# Sessions (Durable, Multi-Runtime)

A **session** is a unit of agent work that survives the process that started
it. It lives as a directory under `$COS_DATA_DIR/sessions/<sid>/` and is
designed so any agent runtime — ours, yours, anyone's — can attach to it,
read what came before, append more turns, and record reversible changes.

That property is the whole point. Other agent platforms keep "session" as
an in-memory object owned by one process; Claw OS keeps it as a few JSON
and JSONL files on disk, with `flock(LOCK_EX)` as the only coordination
primitive between writers. There is no RPC contract to break, no daemon
to keep in sync.

## User-facing CLI

The word "session" never appears in `cos --help`. Users think in terms of
**tasks an agent is doing**. The five verbs are under `cos agent`:

```bash
cos agent ls                       # list every active / paused / failed task
cos agent show <task-id>           # purpose, status, lease, turns, mutations
cos agent stop <task-id>           # politely tell the runtime to wind down
cos agent undo <task-id>           # roll the mutation log back
cos agent resume <task-id>         # let a fresh runtime pick a paused task up
```

`stop` is cooperative — it drops a `stop.requested` sentinel for the live
runtime to notice on its next heartbeat. If no runtime is attached, it
flips the meta status to `paused` immediately so `ls` reflects reality.
`resume` flips `paused → pending`; the agent stack itself (e.g.
`cos agent chat --session <id>`) takes it from there.

## Disk layout

```text
$COS_DATA_DIR/sessions/<sid>/
  meta.json         — purpose, status, role, parent, creator runtime, budget, timestamps
  caps.json         — current CapSet (mutable: caps can be granted/revoked at runtime)
  turns.jsonl       — append-only conversation events, one Turn per line
  mutations.jsonl   — append-only reversible state changes, one MutationRecord per line
  state.json        — opaque per-runtime scratch: {"<runtime>": <value>}
  lease.json        — current owner: {pid, runtime, started_at, heartbeat_at}
  lease.lock        — flock sentinel; the kernel auto-releases it on holder death
  files/inverse/    — pre-mutation byte snapshots referenced by FsWrite/FsDelete
  stop.requested    — present iff `cos agent stop` has been issued
```

### Session id

Format: `ses_<13 hex unix-ms>_<12 hex>`, e.g. `ses_0019e2566eb1f_e71a8d6a8ca4`.

The leading milliseconds make ids chronologically sortable lexically, so
`ls` listings are naturally in creation order.

### Status

```
pending → running → paused → running → done
                          ↘ failed
```

`pending` = created, no agent has ever attached. `running` = a process holds
the lease and is making progress. `paused` = the lease was released
voluntarily (or `cos agent stop` was issued). `done` / `failed` are
terminal — read-only, eligible for archival once the GC retention window
elapses.

## Lease — who is making progress

Exactly one process is the **runner** at any given time, identified by
holding `flock(LOCK_EX | LOCK_NB)` on `lease.lock`. `lease.json` is
informational only — pid, runtime label, `started_at`, `heartbeat_at`.

If the runner crashes, the kernel releases the flock automatically. Any
new process can `try_acquire` and pick the session up exactly where it
left off (turns and mutations are durable on disk). There is no reaper
daemon — the OS is the reaper.

## Schema — turns.jsonl

One JSON object per line, append-only.

```json
{
  "seq": 0,
  "at": "2026-02-01T15:43:21Z",
  "role": "user",
  "content": "find my Q4 invoices",
  "runtime": "cos-agent"
}
{
  "seq": 1,
  "at": "2026-02-01T15:43:23Z",
  "role": "assistant",
  "content": "",
  "runtime": "cos-agent",
  "tool_calls": [
    {"id": "call_1", "name": "fs.glob", "arguments": {"pattern": "invoices/2025-Q4-*.pdf"}}
  ],
  "usage": {"input_tokens": 142, "output_tokens": 31}
}
{
  "seq": 2,
  "at": "2026-02-01T15:43:24Z",
  "role": "tool",
  "content": "[\"invoices/2025-Q4-001.pdf\", \"invoices/2025-Q4-002.pdf\"]",
  "tool_call_id": "call_1"
}
```

| field          | type                        | notes                                                  |
|----------------|-----------------------------|--------------------------------------------------------|
| `seq`          | u64, monotonic, 0-based     | the store assigns it under the same flock as append    |
| `at`           | RFC 3339 UTC string         | auto-stamped if omitted                                |
| `role`         | `user` / `assistant` / `system` / `tool` | kebab-case strings                        |
| `content`      | string                      | free-form; tool calls go in `tool_calls`               |
| `runtime`      | string, optional            | label of the agent that wrote this turn                |
| `tool_calls`   | array of opaque JSON, optional | OpenAI/Anthropic-shape; we don't constrain         |
| `tool_call_id` | string, optional            | for `role:tool`, the assistant call this completes     |
| `usage`        | opaque JSON, optional       | provider's token usage block, verbatim                 |

Readers must tolerate a trailing partial line (the only way the file can
become corrupt is a crash mid-write, which leaves at most one half-written
tail entry).

## Schema — mutations.jsonl

Append-only inverse-action log. `cos agent undo <sid>` walks this file
newest-first.

```json
{"seq": 0, "at": "2026-02-01T15:43:30Z", "runtime": "cos-agent", "mutation": {"kind": "fs-write", "path": "/home/jay/notes.md", "prev_blob": "8d4c..."}}
{"seq": 1, "at": "2026-02-01T15:43:31Z", "runtime": "cos-agent", "mutation": {"kind": "fs-rename", "from": "/home/jay/old.md", "to": "/home/jay/new.md"}}
```

Mutation variants (kebab-case `kind` discriminator):

| `kind`              | extra fields                                  | undoable how                                        |
|---------------------|-----------------------------------------------|-----------------------------------------------------|
| `fs-write`          | `path`, `prev_blob` (nullable string)         | restore bytes from `files/inverse/<prev_blob>.bin`; if null, delete the file (it didn't exist before) |
| `fs-delete`         | `path`, `blob_id` (string)                    | recreate file from blob                              |
| `fs-rename`         | `from`, `to`                                  | rename `to` back to `from`                           |
| `credential-store`  | `namespace`, `key`                            | revoke (Phase 4)                                     |
| `credential-revoke` | `namespace`, `key`, `prev_blob` (string)      | restore (Phase 4)                                    |
| `opaque`            | `verb`, `forward`, `inverse` (both JSON)      | not auto-rolled-back; surfaced to user for review    |

Inverse blobs (`files/inverse/<id>.bin`) are written tmp+rename atomically
so a crash mid-stash never leaves a torn blob. Blob ids are UUIDv4 simple
hex (32 lowercase chars, no dashes); both Rust and Python use the same
scheme.

## Cross-runtime handover

This is the load-bearing claim: **a third-party runtime can attach to a
session our `cos agent` runtime started, read its turns, and append new
ones — without ever shelling out to `cos`.** Python is the shipped
reference implementation; other runtimes must implement the same
protocol before writing.

The contract is exactly the file format above. We ship a reference
implementation at [`claw-os-sdk/python/src/claw_os_sdk/claw_os_session.py`](../../claw-os-sdk/python/src/claw_os_sdk/claw_os_session.py)
(~330 lines, no third-party deps) that:

- lists durable sessions (skipping `.archive`, half-written, or
  non-matching dirs — same rules as the Rust kernel),
- opens one and reads `meta.json` / `lease.json` / `turns.jsonl` /
  `mutations.jsonl`,
- appends turns under `flock(LOCK_EX)` so the seq counter and the line
  write happen atomically,
- records `fs-write` / `fs-delete` / `fs-rename` / `opaque` mutations
  with proper inverse-blob stashing.

JSONL is the only sequence source of truth; there is no counter
sidecar. Every writer must, under the same exclusive flock:

1. Validate complete records have contiguous `seq` values `0..N-1`.
2. Repair only one invalid trailing fragment by truncating it; any
   complete corrupt/missing/duplicate sequence is fatal.
3. Treat a valid final JSON record without `\n` as complete and add the
   separator.
4. Append `seq=N` with a full-write loop and fsync before unlocking.

Readers take a shared flock and tolerate only an invalid trailing
fragment. Mid-file corruption is surfaced rather than silently skipped.

Minimal example:

```python
from claw_os_session import Session

s = Session.open("ses_0019e2566eb1f_e71a8d6a8ca4")

# Read what came before — including turns from any other runtime.
for t in s.turns()[-5:]:
    print(t["role"], t.get("runtime", "?"), t["content"])

# Append your own turn. Tag it with a runtime label so the audit trail
# shows it didn't come from the system agent.
s.append_turn(
    role="assistant",
    content="Continuing your invoice scan from a third-party agent.",
    runtime="my-bot-py",
)

# Record a reversible file edit. `prev_bytes` is what the file held
# before you changed it — claw-os stores it as an inverse blob so
# `cos agent undo <sid>` can put it back.
s.record_fs_write("/home/jay/report.md", prev_bytes=old, runtime="my-bot-py")
```

The reverse direction works too: any turn or mutation a Python agent
writes is visible to the Rust kernel via `session::iter_turns` /
`session::iter_mutations`, and is rollback-able via
`session::rollback`. The cross-runtime handover tests in
`core/src/session/tests.rs` (`cross_runtime_*`) demonstrate this end
to end.

## What goes in `turns.jsonl` vs `state.json`

- `turns.jsonl` — durable, ordered, **shared across runtimes**. Anything
  another agent picking up the session needs to see to continue.
- `state.json` — opaque per-runtime scratch (compiled prompt prefixes,
  vector cache pointers, planner-internal queues). Other runtimes are
  free to ignore it. Stored as `{"<runtime-label>": <opaque value>}` so
  multiple runtimes can coexist without stepping on each other.

Rule of thumb: if a different agent picking up the session needs to see
it to continue, it belongs in `turns.jsonl`.

## GC and archival

Sessions in `done` or `failed` status get archived to
`$COS_DATA_DIR/sessions/.archive/<sid>.zip` after a retention window;
the original directory is removed. `Session.list()` and `cos agent ls`
both skip the `.archive` directory automatically.

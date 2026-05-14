# Improvements

Tracked: outstanding design-level improvements to Claw OS. Each entry
states the **problem**, the **cause**, and the **proposed fix** with
concrete file-level pointers. Entries graduate to commits/PRs and
then get crossed off here.

---

## Memory subsystem: long-horizon scaling

### Background — what each store actually does

Claw OS ships four memory stores. They are **not** redundant; each
covers a different access pattern. The earlier draft confused
"the curator writes to `USER.md`" — that is wrong; the curator
writes to `MEMORY.md`. `USER.md` is reserved for **LLM-initiated**
appends via the `cos_memory` tool.

| Store | Physical | Writer | When | Injected into prompt? | Read path |
| --- | --- | --- | --- | --- | --- |
| `USER.md` | `notes/USER.md` | **LLM only** (`cos_memory append USER.md`) | LLM judges a fact as stable user-persona info | ✅ every turn (≤32 KB cap) | direct file read |
| `MEMORY.md` | `notes/MEMORY.md` | (a) LLM via `cos_memory`, (b) **curator** auto-extract | curator: background after every final answer; LLM: at its discretion | ✅ every turn (≤32 KB cap) | direct file read |
| `memory.db` | SQLite + FTS5 | **runtime auto-record** | every message (user / assistant / tool) | ❌ | `cos_recall` (keyword) |
| `semantic.db` | SQLite + 1536-dim vectors | **runtime auto-index** (calls embed API) | every non-empty message | ❌ | `cos_recall_semantic` (cosine) |

Two orthogonal axes describe the design:

```
                    │ auto-injected into prompt │ on-demand via LLM tool
─────────────────────┼───────────────────────────┼────────────────────────
human-readable (md) │ USER.md / MEMORY.md       │ —
machine-indexed     │ —                         │ memory.db / semantic.db
```

And two semantic tiers for the markdown:
- **`USER.md`** = *about the person* — durable preferences, persona,
  workflow. Updated rarely.
- **`MEMORY.md`** = *about the task/context* — current projects,
  things just learned. Updated often.

### The actual scaling problem

After running the numbers for a full year of daily use:

| usage | msg/day | `memory.db` 1y | `semantic.db` 1y | vector search latency |
| --- | --- | --- | --- | --- |
| light (10 turns) | ~30 | ~10 MB | ~75 MB | <5 ms |
| medium (50 turns) | ~150 | ~55 MB | ~380 MB | ~20 ms |
| heavy (200 turns) | ~600 | ~220 MB | ~1.5 GB | ~60 ms |

`memory.db` and `semantic.db` **do not need time-bucket partitioning**.
FTS5 is O(log n); brute-force cosine over 100 K vectors stays
millisecond-class on a modern laptop. A soft TTL at the very end
(e.g. purge raw messages past 1 y) is housekeeping, not urgent.

**The real bottleneck is `MEMORY.md` and `USER.md`.** They are injected
into the system prompt **every single turn**. Per-turn token cost
scales linearly with their size — a 32 KB file is ~8 K tokens. After
a year of unbounded appends, a heavy user could be paying
8 K tokens/turn just to re-state historical facts.

`MAX_NOTE_CHARS_FOR_PROMPT = 32_768` in `core/src/agent/memory/notes.rs`
caps the read-into-prompt size, but once we hit that cap the
curator effectively stops adding value (older facts get truncated
silently with no compaction strategy).

### Proposed improvements

#### M1 — Curator: semantic dedup before append

**Problem.** Curator already truncates at `MAX_FACTS` but appends
near-duplicates verbatim ("user prefers vim" + "user's editor is
vim"). MEMORY.md fills with redundant lines.

**Fix.** Before `notes.append(MEMORY_FILE, …)`, embed each candidate
fact and compute cosine vs. existing facts in MEMORY.md. If
similarity > 0.92, **merge** (LLM rewrite of the existing line)
instead of appending a new one.

**Files.**
- `core/src/agent/memory/curator.rs:680-740` (write path)
- New helper using `model::tasks::embed::build_default()` (already
  default-on for openai-shape providers).

**Acceptance.** A test session that repeats "I use vim" 5 times
produces exactly **one** fact line in MEMORY.md.

#### M2 — MEMORY.md rolling archive

**Problem.** When MEMORY.md crosses the 32 KB hard cap, the oldest
facts get truncated from the prompt with no record kept.

**Fix.** When `MEMORY.md` size > 24 KB (75% of cap), curator rolls
the oldest ~8 KB block into `journals/MEMORY-YYYY-MM-DD.md`, then
prepends a one-paragraph LLM-summary of that block to the top of
the live `MEMORY.md`. Live MEMORY.md stays bounded; older fidelity
is on disk if needed via `cos_recall_journal <date>`.

**Files.**
- `core/src/agent/memory/curator.rs` — new `compact_memory()` step
  in the curate pipeline, runs after `extract_facts` when over
  threshold.
- `core/src/agent/memory/notes.rs` — new `journals_dir()` helper +
  `archive_block(name, content)`.

**Acceptance.** A session that pushes MEMORY.md past 24 KB results
in (a) live file ≤ 24 KB with a summary header, (b) one new
`journals/MEMORY-*.md` file, (c) total facts preserved across the
two files modulo summarisation.

#### M3 — `cos_recall_journal <date>` LLM tool

**Problem.** Once M2 archives blocks, the LLM needs a way to pull a
specific archived day on demand (e.g. "what did I work on last
Tuesday?").

**Fix.** New tool in `core/src/agent/tools/cos_proxy/` that:
- `cos_recall_journal --date 2026-05-06` → returns
  `journals/MEMORY-2026-05-06.md` content
- `cos_recall_journal --list --since 7d` → lists available
  journal files
- `cos_recall_journal --search "v8 build" --since 30d` → grep
  across archived journals

**Files.**
- New `core/src/agent/tools/cos_proxy/recall_journal.rs`
- Register in `core/src/agent/tools/cos_proxy/mod.rs`
- Add to `core/src/agent/tools/registry.rs`

**Acceptance.** LLM can reliably retrieve "what was MEMORY.md on
2026-05-06" via the tool, and the response stays under the
per-tool response cap.

#### M4 — `USER.md` dedup helper

**Problem.** `cos_memory append USER.md` is unconditional. The LLM
sometimes appends the same preference twice across distant
sessions.

**Fix.** Add `--dedup` flag to `cos_memory append` (default on
for `USER.md`). The tool reads the existing file, runs a cheap
substring + token-set comparison against each existing line, and
skips the append if a near-duplicate exists.

**Files.**
- `core/src/agent/tools/cos_proxy/memory.rs` — append path.

**Acceptance.** A test that issues two near-identical
`cos_memory append USER.md` calls leaves USER.md with exactly one
line.

#### M5 — Raw-store TTL (housekeeping, low priority)

**Problem.** `memory.db` and `semantic.db` grow forever. Not
urgent (see scaling numbers above) but eventually should be
managed.

**Fix.** Add `agent.memory_ttl_days` and
`agent.semantic_ttl_days` (default `0` = never). When set, a
daily janitor pass deletes rows older than the TTL. Curator
should have already promoted important facts to `MEMORY.md` /
`USER.md` by then.

**Files.**
- `core/src/agent/memory/sqlite_fts.rs` — new `purge_older_than(ms)`.
- `core/src/agent/memory/semantic.rs` — same.
- New cron-style entry in the agent service init.

**Acceptance.** With `memory_ttl_days = 30`, rows older than 30 d
disappear on the next janitor run; recent rows untouched.

### Out of scope (decided)

- **Time-bucket partitioning of `memory.db` / `semantic.db`** —
  unnecessary at realistic data volumes (1.5 GB / year worst case;
  search stays sub-100 ms). The per-day discriminant already
  lives on the `timestamp_ms` column.
- **Replacing `USER.md` with structured fields** — markdown stays;
  it's intentionally agent-readable.
- **HNSW / IVF index over `semantic.db`** — defer until brute-force
  cosine becomes the bottleneck (500 K+ vectors).

### Ordering

Suggested implementation order:
1. **M1** (dedup before append) — biggest immediate win, no schema change.
2. **M2** (rolling archive) — directly removes the unbounded growth.
3. **M3** (recall_journal tool) — closes the loop for retrieval.
4. **M4** (USER.md dedup) — quality-of-life.
5. **M5** (TTL janitor) — housekeeping, defer until data actually justifies it.

---

## System introspection — making the agent actually live in the OS

### Problem

The agent is supposed to be an OS-resident assistant on Linux/COSMIC,
but for the first cut it could only answer roughly the same questions
a shell user with read access to `/proc` could answer manually
(`info`, `env`, `resources`, `uptime`, `proc`, `mounts`, `net`,
`cgroup`). Everything else — *"which process is eating CPU right
now"*, *"how hot is the chip"*, *"what just crashed"*, *"who's on
port 8080"* — required ad-hoc shell pipelines via `cos_sandbox`,
which is the LLM equivalent of telling a doctor to bring their own
stethoscope.

### Cause

`core/src/sysinfo.rs` exposed only the cheapest `/proc` reads.
Anything that needed (a) two-sample diffing (CPU%, IO/sec), (b)
shelling out to a system tool (journalctl, systemctl, apt, dmesg,
coredumpctl, who), or (c) parsing structured `/sys` hierarchies
(thermal, power_supply, hwmon) was missing entirely.

The `cos agent doctor` command was also CLI-only — even though it
returns JSON and its `doctor_cmd` already matches the cos primitive
signature, the LLM had no tool entry for it.

### Done (Linux-only)

`core/src/sysinfo.rs` now ships **24 sub-commands** under the
`cos_sysinfo` tool:

| Group | Commands |
|---|---|
| identity | `info`, `env`, `uptime`, `who`, `desktop` |
| load / health | `resources`, `loadavg`, `sensors`, `cgroup` |
| processes | `proc`, `top`, `threads`, `port` |
| network | `net`, `net_rate` |
| storage | `mounts`, `disk_io`, `largest_files` |
| logs | `journal`, `dmesg` |
| systemd | `services`, `failed_units`, `coredumps` |
| packages | `pkg_updates` |

Key behaviours:

- **`top`** — two-sample `/proc/<pid>/stat` diff, returns a real
  `cpu_percent` per process (configurable `--interval`, `--top`,
  `--by cpu|mem`).
- **`threads <pid>`** — walks `/proc/<pid>/task/`, returns per-TID
  state and CPU.
- **`port <port>`** — cross-references `/proc/net/{tcp,tcp6,udp,udp6}`
  with `/proc/<pid>/fd/socket:[inode]` to map a port to owning PIDs.
- **`sensors`** — reads `/sys/class/power_supply/` (battery state,
  capacity, remaining-runtime estimate, AC adapters),
  `/sys/class/thermal/` (thermal zones), and `/sys/class/hwmon/`
  (fans + extra temps). All values in canonical SI / Celsius.
- **`journal`** — wraps `journalctl -o json` with `--unit`,
  `--since`, `--lines`, `--priority`, `--kernel`. Returns parsed
  JSON entries with a stable schema (timestamp / unit / priority /
  pid / comm / message).
- **`services`** — wraps `systemctl list-units --output=json` with
  `--failed-only`, `--type`, `--state`.
- **`coredumps`** — `coredumpctl list --json=short` with a raw-text
  fallback for older systemd.
- **`disk_io` / `net_rate`** — two-sample `/proc/diskstats` and
  `/proc/net/dev` for kB/s rates.
- **`largest_files <path> [--top N --min-mb N]`** — bounded-size
  min-heap walker; stays on one filesystem like `find -xdev`.

Also: **`cos_doctor`** — a new top-level LLM tool that exposes
`cos agent doctor` to the model. Flags only; the `command` arg is
ignored (single-shot). Output JSON has the `status: ok|warn|fail`
rollup the CLI already produces. Wired via the standard
`PrimitiveSpec` pattern in `core/src/agent/tools/cos_proxy/mod.rs`.

Files touched:
- `core/src/sysinfo.rs` — +16 commands, +~1100 lines, +tests.
- `core/src/agent/tools/cos_proxy/mod.rs` — extended `cos_sysinfo`
  spec; added `cos_doctor` `PrimitiveSpec`.
- `core/src/agent/doctor_cli.rs` — `doctor_primitive` shim.
- `core/src/agent/tools/registry.rs` — assert `cos_doctor` is
  registered.

### Deferred / still open

These are real gaps but are bigger architectural pieces that
deserve their own PRs:

#### S1 — Full desktop state (windows, displays, clipboard)

`cos_sysinfo desktop` currently only returns XDG env-var hints.
Real "what window is active / what's on my clipboard / what
monitors are plugged in" requires:

- A Wayland protocol client (most likely a thin wrapper around
  `wlr-foreign-toplevel-management-unstable-v1` or COSMIC's
  equivalent) to enumerate toplevels.
- A D-Bus client for COSMIC's display config service to enumerate
  monitors at the protocol layer (not just `cosmic-randr-shell`,
  which is x86-only).
- An opt-in clipboard reader (Wayland's `wl_data_device` plus the
  `clipboard-control` proposal). Must default to **off** with an
  explicit per-session toggle — clipboard content is sensitive.

Suggested home: a new `core/src/sysinfo/desktop.rs` (split the
module when this lands).

#### S2 — Event subscriptions (push, not poll)

Today everything in `cos_sysinfo` is request/response: the LLM
asks, the kernel reads, the answer flows back. There is no way for
the OS to **wake the agent** when something happens. The natural
event sources:

- **inotify** for file changes (`apps/notify` already uses this
  shape for desktop notifications but not for the agent).
- **udev** for hot-plug events (USB, displays, batteries).
- **systemd D-Bus signals** — `JobNew`, `JobRemoved`, unit state
  transitions, `PrepareForSleep`.
- **journalctl `--follow`** for matching-priority log lines.
- **`/proc/<pid>` death watches** via `pidfd_open` + epoll.

The right shape is probably an `AgentEventBus` (running inside
the agent service) that fans these signals into LLM-readable
inbox entries — similar to how `cos cron` already persists jobs.
This pairs naturally with the durable-session work in
`core/src/session/` (Phase 6 handover) so the agent can wake from
checkpoint and immediately ingest backlog.

#### S3 — Reversible signal log

Today `cos_proc signal/kill` is fire-and-forget. If the LLM kills
the wrong PID there is no breadcrumb. Proposal:

- New file `$COS_DATA_DIR/agent/signal_log.jsonl`.
- Every `proc.signal` invocation appends
  `{ts, pid, signo, comm, cmdline, killer_session}` before sending
  the signal.
- New `cos_sysinfo signal_log [--lines N]` to surface recent
  kills to the agent ("what did I just kill?").
- Optional: pair with `core/src/checkpoint` so the agent can
  `restart_last_killed --within 5m` (re-spawn the cmdline). True
  rollback isn't possible — once a process is gone its in-memory
  state is gone — but re-spawning the same cmdline is good enough
  for daemons.

Hook point: `core/src/proc.rs::cmd_signal` and
`core/src/policy.rs` (where `proc.signal` is gated).

#### S4 — Two-sample everywhere

`top`, `disk_io`, `net_rate` already sample twice. Apply the same
pattern to CPU usage per-cgroup, per-uid network usage
(`/proc/net/netstat` + `/proc/net/sockstat`), and per-pid IO
(`/proc/<pid>/io`). Cheap and high-signal.


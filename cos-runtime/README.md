# `cos-runtime` — internal OS runtime for claw-os bundled apps

> **Not a developer SDK.** Third-party Linux apps written for claw-os do not
> need this crate / package. If you only want to **call** the system LLM or
> expose tools to the agent, use [`claw-os-sdk`](../claw-os-sdk/) instead.

This directory holds the helpers the claw-os kernel uses to talk to the apps
it bundles under `apps/*` and to the cosmic desktop GUI binaries under
`desktop/*`. The split exists because the public SDK should be small,
documented, and AI-focused, whereas the runtime here is OS implementation
detail:

| Module (Rust) | Module (Python) | Purpose |
|---|---|---|
| `cos_runtime::policy` | `cos_runtime.policy` | Shell out to the hidden policy bridge for self-gating capability enforcement |
| `cos_runtime::fs` | (not applicable) | Route every `fs.*` op through `cos app fs <verb>` so audit / snapshots / caps apply |
| `cos_runtime::exec` | (not applicable) | Route every `exec.*` op through `cos app exec <verb>` |
| `cos_runtime::pkg` | (not applicable) | Route `pkg.*` ops similarly |
| `cos_runtime::notify` | (not applicable) | Route `notify.*` ops similarly |
| `cos_runtime::net` | (not applicable) | Route `net.*` ops similarly |
| `cos_runtime::ask_claw` | (not applicable) | Serialize bounded typed desktop context and launch the Agent overlay through supervised `exec.start` with explicit stdin |
| (not applicable) | `cos_runtime.snapshot` | Copy-on-write before every gated fs mutation |

These modules talk wire-v1 too, but they're the *kernel side* of that wire —
the consumers are the bundled apps in this repo, not external apps.

## Why a separate crate / package

- **`publish = false`** for the Rust crate; no `pyproject.toml` for the Python
  package. Neither shows up on crates.io or PyPI.
- The OS installs both packages to `/usr/lib/cos/python/cos_runtime/` and
  embeds the Rust crate via `path = ` in workspace deps. There is no scenario
  where someone installs `cos-runtime` separately.
- Importing `cos_runtime` from a non-claw-os process **will fail loudly**
  (`cos` binary not on PATH, no `COS_SESSION` env, etc.) — by design.

## Ask Claw desktop integration

Bundled desktop apps implement `cos_runtime::ask_claw::Context` on a narrow
app-local `Serialize` type, then call `ask_claw::launch`. The runtime inserts
the app identity, serializes with `serde_json`, rejects non-object/reserved or
larger-than-32-KiB contexts, wraps it in a typed activation, and sends it over
the explicit bounded `exec.start` stdin channel. The call runs on a dedicated
launcher thread and the intermediary `cos` process has a five-second deadline
with kill/reap on timeout. The exec app forwards the payload once through a
sealed anonymous memfd connected to a transient Agent UI's stdin, without
creating process-registry rows or stdout/stderr artifacts. No context content
enters argv, audit records, the process registry, the environment, or the
filesystem.

The Agent UI imports the same activation type and CLI parser from this module.
It reads stdin only when `--context-stdin` is explicitly present, enforces the
activation and context bounds, validates the typed activation and embedded
context, and closes stdin. Context-bearing overlays deliberately run as
independent transient instances rather than forwarding plaintext through the
unauthenticated well-known D-Bus name; context-free global shortcut activation
continues to use the single instance. Legacy external `--context` input remains
accepted in the same transient mode, but the shared launcher only emits
`--context-stdin`. Supplying both sources rejects the entire activation.

Anonymous handoff fails closed unless Linux Yama
`kernel.yama.ptrace_scope >= 2`. The host, `cos`, exec app, and Agent UI are
marked non-dumpable before they read or forward the payload. This is required
because memfd seals prevent mutation, not reads by an otherwise ptrace-capable
same-UID peer.

Keep these typed adapters in each app's `claw_glue` module. Reducers should
only select the user intent and pass the already-visible page, query, path, or
terminal output fields; they must not build JSON or know the Agent UI command.
The Terminal adapter uses the runtime's encoded-size predicate to drop oldest
lines first and then truncate at a UTF-8 boundary, retaining `app`, `mode`,
`cwd`, and `truncated` metadata while still opening the overlay.

Normal `cos_runtime::exec::start` remains registry-backed and returns an opaque
launch id plus PID/start-time metadata. Stops resolve that identity, verify the
live process start time, and signal through pidfd; numeric PID arguments remain
a compatibility input but are subject to the same registry and identity checks.
Both normal and stdin-bearing start APIs insert `--` before child argv so child
flags cannot be consumed by the `cos` or exec option parsers.

## Relationship to `claw-os-sdk`

```
┌─────────────────────────────────────────┐
│  claw-os-sdk     (public, published)    │
│  ─ ai · tools · serve · generated       │
└──────────────▲──────────────────────────┘
               │   uses Envelope, generated types
┌──────────────┴──────────────────────────┐
│  cos-runtime    (internal, OS-bundled)  │
│  ─ policy · snapshot · fs · exec · …    │
└─────────────────────────────────────────┘
```

`cos-runtime` depends on `claw-os-sdk` for the typed wire envelope, never the
other way around.

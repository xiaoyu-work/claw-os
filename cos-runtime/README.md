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
| `cos_runtime::ask_claw` | (not applicable) | Serialize bounded typed desktop context, stage it privately, and launch the Agent overlay through supervised `exec.start` |
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
larger-than-32-KiB contexts, and writes the result to a uniquely created `0600`
file under the caller's private `$XDG_RUNTIME_DIR/claw-os-ask-claw/` directory.
Only the path is passed through `exec.start` and recorded in process metadata;
the JSON payload never enters argv, the process registry, or `/proc/*/cmdline`.

The Agent UI imports the same activation type and CLI parser from this module.
It validates that the file is a direct child of the private runtime directory,
opens it with no-follow semantics, verifies type, owner, mode, and size, unlinks
it before reading, and rejects malformed context JSON. Failed launches remove
their staged file. Abandoned files older than ten minutes are removed before a
later launch. Legacy external `--context` input remains accepted, but the
shared launcher only emits `--context-file`.

Keep these typed adapters in each app's `claw_glue` module. Reducers should
only select the user intent and pass the already-visible page, query, path, or
terminal output fields; they must not build JSON or know the Agent UI command.
The Terminal adapter uses the runtime's encoded-size predicate to drop oldest
lines first and then truncate at a UTF-8 boundary, retaining `app`, `mode`,
`cwd`, and `truncated` metadata while still opening the overlay.

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

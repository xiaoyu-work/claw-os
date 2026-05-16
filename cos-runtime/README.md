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
| `cos_runtime::policy` | `cos_runtime.policy` | Shell out to `cos perms check` for self-gating capability enforcement |
| `cos_runtime::fs` | (not applicable) | Route every `fs.*` op through `cos app fs <verb>` so audit / snapshots / caps apply |
| `cos_runtime::exec` | (not applicable) | Route every `exec.*` op through `cos app exec <verb>` |
| `cos_runtime::pkg` | (not applicable) | Route `pkg.*` ops similarly |
| `cos_runtime::notify` | (not applicable) | Route `notify.*` ops similarly |
| `cos_runtime::net` | (not applicable) | Route `net.*` ops similarly |
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

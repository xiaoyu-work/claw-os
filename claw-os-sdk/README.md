# claw-os-sdk

Official SDK for building apps that run on Claw OS.

This directory is the **single source of truth** for the developer-facing
surface of Claw OS. Everything that's not in here is internal kernel
plumbing or vendored upstream code — third-party app developers should
need nothing more than what this folder exports.

## Layout

```
claw-os-sdk/
├── wire/                Wire protocol — language-agnostic
│   ├── v1/
│   │   ├── README.md           protocol overview
│   │   ├── envelope.schema.json    common envelope shape
│   │   ├── perms.schema.json       cos perms check / grant
│   │   ├── ai.schema.json          cos ai chat / tool / embed / ...
│   │   ├── tool.schema.json        catalog tool invocation
│   │   ├── app.schema.json         cos app <id> <verb>
│   │   └── manifest.schema.json    app.json schema
│   └── codegen.py        reads wire/v1/*.schema.json → emits
│                         typed bindings into rust/python/node/go.
│
├── rust/                Rust SDK (cargo crate `claw-os-sdk`)
│   ├── Cargo.toml
│   ├── README.md
│   ├── examples/
│   └── src/
│       ├── lib.rs               top-level re-exports + transport
│       ├── envelope.rs          common envelope parse / error
│       ├── ai.rs                ai::chat / ai::embed / ai::image_generate / ...
│       ├── tools.rs             tools::call / tools::catalog
│       └── generated.rs         codegen output (envelope types)
│
├── python/              Python SDK (pip package `claw-os-sdk`)
│   ├── pyproject.toml
│   ├── README.md
│   └── src/claw_os_sdk/
│       ├── __init__.py
│       ├── ai.py, tools.py, serve.py, claw_os_session.py
│       └── generated.py
│
├── node/                Node SDK (npm package `@claw-os/sdk`)
│   ├── package.json
│   ├── README.md
│   └── src/
│       ├── index.ts             top-level re-exports
│       ├── transport.ts         subprocess transport
│       ├── ai.ts, tools.ts
│       └── generated.ts         codegen output
│
├── go/                  Go SDK (module github.com/xiaoyu-work/claw-os-sdk/go)
│   ├── go.mod
│   ├── README.md
│   ├── transport.go
│   ├── ai.go, tools.go
│   └── generated.go
│
└── README.md
```

> The OS-internal `policy`, `snapshot`, and Rust `fs / exec / pkg /
> notify / net` helpers used by the bundled claw-os apps live in the
> sibling [`cos-runtime/`](../cos-runtime/) tree, not here. Third-party
> Linux app developers do **not** need that package — it's only
> referenced by code that ships inside the OS itself.

## The model

Every SDK in every language is a **thin client** over the same
**wire protocol v1**, which is the JSON envelope that `cos` (the
kernel CLI) reads from argv and writes to stdout.

```
┌──────────────────────┐
│  your app (any lang) │
└──────────┬───────────┘
           │ language-native call: ai.chat("…")
           ▼
┌──────────────────────────────────────────────────────────┐
│  claw-os-sdk (rust / python / node / go)                 │
│  - typed structs (generated from wire/v1/*.schema.json)  │
│  - language-idiomatic API surface                        │
└──────────┬───────────────────────────────────────────────┘
           │ subprocess: cos ai chat --app <id> --prompt …
           ▼
┌──────────────────────────────────────────────────────────┐
│  cos                  (Rust binary, in /usr/bin)         │
│  - caps gate, audit, budget, safety, provider routing    │
└──────────────────────────────────────────────────────────┘
```

The reason for the subprocess transport: **identity, audit, and
session context are inherited from process ancestry** (kernel-spawned
parent → app process → cos child). A pure-library binding can't claim
"App X is making this call" without that lineage.

A v2 socket transport is planned (see `wire/v2-design.md`) to bring
per-call latency down from ~50 ms to µs-class, but the same envelopes
flow over either transport.

## What to generate vs hand-write

Codegen handles the **boring** part:

| Generated         | Hand-written                                |
|-------------------|---------------------------------------------|
| Request / reply struct types | The transport (how to spawn `cos`) |
| Error code enum   | The high-level wrappers (`ai.chat("…")`)    |
| Manifest schema bindings | Examples, docs, language-idiomatic helpers |

Run codegen with:

```sh
cd claw-os-sdk
python3 wire/codegen.py            # writes generated.* into each language tree
```

## Versioning

- **Wire protocol** has its own version (`v1`, `v2`, …) bumped only on
  envelope-breaking changes.
- **Each language SDK** has its own SemVer (Cargo / pip / npm / Go modules).
  Their CHANGELOGs note which wire version they target.
- All SDKs handshake on start-up: each library calls `cos --version` and
  refuses to operate against a `cos` binary that's older than the
  minimum wire-version it requires.

## Compatibility

claw-os is pre-1.0. Breaking changes in the wire protocol are allowed
and announced in `wire/CHANGELOG.md`. SDKs may pin to a specific wire
version and refuse to run against incompatible kernels.

## Status (2026-05-15)

| Component | Status |
|-----------|--------|
| `wire/v1` schemas | Initial draft |
| `wire/codegen.py` | Initial — emits Rust + Python; Node + Go are placeholder generators |
| `rust/` | Moved from `crates/claw-bridge`; adds `ai`, `tools`. The `policy / fs / exec / pkg / notify / net` modules moved on into `cos-runtime/`. |
| `python/` | Moved from `apps/_lib`; published as `claw-os-sdk`. The internal `policy` / `snapshot` helpers moved on into `cos-runtime/python/`. |
| `node/` | Scaffold only — transport stub, hand-rolled API to come |
| `go/`   | Scaffold only — transport stub, hand-rolled API to come |

See each language's `README.md` for usage.

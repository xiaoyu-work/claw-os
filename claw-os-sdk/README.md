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
│   │   ├── perms.schema.json       capability decision envelope
│   │   ├── ai.schema.json          AI wire schema (kernel protocol)
│   │   ├── budget_show.schema.json cos agent budget show reply
│   │   ├── tool.schema.json        catalog tool invocation
│   │   ├── tool_catalog.schema.json catalog tool list
│   │   ├── mcp_call_context.schema.json authenticated App-call identity
│   │   ├── contract.json           validators + stable error codes
│   │   ├── app.schema.json         cos app <id> <verb>
│   │   └── manifest.schema.json    app.json schema
│   └── codegen.py        reads wire/v1 schemas + contract → emits
│                         typed validators into rust/python/node/go
│                         and MCP error-code modules.
│
├── rust/                Rust SDK (cargo crate `claw-os-sdk`)
│   ├── Cargo.toml
│   ├── README.md
│   ├── examples/
│   └── src/
│       ├── lib.rs               top-level re-exports + transport
│       ├── envelope.rs          common envelope parse / error
│       ├── ai.rs                gated chat API
│       ├── mcp/                 native App MCP server runtime
│       ├── tools.rs             tools::call / tools::catalog
│       └── generated.rs         codegen output (envelope types)
│
├── python/              Python SDK (pip package `claw-os-sdk`)
│   ├── pyproject.toml
│   ├── README.md
│   └── src/claw_os_sdk/
│       ├── __init__.py
│       ├── ai.py, tools.py, mcp.py, claw_os_session.py
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
├── go/                  Go SDK (module github.com/xiaoyu-work/claw-os/claw-os-sdk/go)
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
> sibling [`cos-runtime/`](../cos-runtime/) tree, not here. That
> package is not published or supported for third-party use, and none
> of those helpers is exported by a public SDK. Public SDK operations
> are capability-checked by the `cos` kernel when they run.

## The model

Every SDK consumes the same **wire protocol v1**. Outbound AI and
system calls use the JSON envelope that `cos` writes to stdout.
Inbound App tools use private MCP stdio owned by the App Host, with a
Gateway-authenticated call context that is never taken from tool
arguments.

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
           │ subprocess: cos ai chat --app <id> --prompt-file <0600 temp>
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

## AI support

Across Rust, Python, Node, and Go, the stable hand-written AI surface is
`chat` / `chat-untrusted`. Supplying origin `external-content`
automatically selects `ai.chat.untrusted`.

The existing embed, image, vision, audio, and video helper names and
signatures remain for compatibility. They are deprecated, experimental,
and currently unsupported, and always return a language-specific typed
unsupported error before invoking `cos`.

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
python3 wire/codegen.py --check    # verifies all generated outputs are current
```

## Versioning

- **Wire protocol** has its own version (`v1`, `v2`, …) bumped only on
  envelope-breaking changes.
- **SDK bindings** share one SemVer and are released together so Python, Node,
  Rust, and Go always target the same generated wire contract.
- All SDKs handshake on start-up: each library calls `cos --version` and
  refuses to operate against a `cos` binary that's older than the
  minimum wire-version it requires.

## Compatibility

claw-os is pre-1.0. Breaking changes in the wire protocol are allowed
and announced in `wire/CHANGELOG.md`. SDKs may pin to a specific wire
version and refuse to run against incompatible kernels.

Generated decoders follow JSON Schema's mathematical-integer semantics:
finite `1`, `1.0`, and `1e0` values are equivalent integers. Bounds are
checked after type validation using lossless decimal-rational lexemes, and all
SDK adapters preserve accepted values. Node exposes u64-domain values as
`bigint` only when they cannot be represented safely as `number`.

Unrestricted JSON payloads use a stable lossless public model:

- Rust uses `serde_json::Value` and `serde_json::to_string`.
- Python uses ordinary JSON values plus `Decimal`/`WireDecimal`; use
  `generated.encode_wire_json`.
- Node uses ordinary JSON values plus `bigint` and `WireDecimal`; use
  `generated.stringifyWireJson`.
- Go uses `any` plus `json.Number` and the standard `encoding/json` encoder.

Tool-call inputs may be any JSON value, including explicit `null`, scalars,
and arrays. Passing a proposed input directly to the matching tool invocation
preserves its exact wire representation.
Compact exponents that are unsafe to materialize remain `WireDecimal`
wrappers; serializers never expand them in memory. Node serialization rejects
non-finite and unsafe integer-valued native `number` values—use `bigint` or
`WireDecimal` instead.

## Releases

SDK releases are published through GitHub rather than PyPI, npm, or crates.io.
The manually dispatched **Publish SDK Release** workflow validates a single
SemVer across all language manifests, regenerates wire bindings, runs every
language test suite, and creates:

- `sdk-v<version>` for the GitHub Release and Python/Node/Rust consumers;
- `claw-os-sdk/go/v<version>` for Go module resolution;
- Python wheel and source distribution;
- Node package tarball;
- Rust `.crate` archive;
- Go and complete SDK source archives;
- `SHA256SUMS`.

Install from an immutable release artifact or tag, never from `main`.

## Status (2026-05-15)

| Component | Status |
|-----------|--------|
| `wire/v1` schemas | Initial draft |
| `wire/codegen.py` | Emits deterministic Rust, Python, Node, Go, and MCP validation bindings |
| `rust/` | Public `ai`, `tools`, and native App `mcp` modules. The `policy / fs / exec / pkg / notify / net` modules live in `cos-runtime/`. |
| `python/` | Moved from `apps/_lib`; packaged as `claw-os-sdk`. The internal `policy` / `snapshot` helpers moved on into `cos-runtime/python/`. |
| `node/` | Built out — `ai`, `tools`, `gui` over wire v1, with tests |
| `go/`   | Built out — `ai`, `tools`, `gui` over wire v1, with tests |

See each language's `README.md` for usage.

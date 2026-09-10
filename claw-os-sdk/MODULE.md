# SDK Module

## Purpose

`claw-os-sdk/` defines the public, language-neutral app/agent contract and its
Rust, Python, Node, and Go bindings.

## Responsibilities

- Maintain versioned wire types and operation/capability schemas.
- Define the MCP-first App service contract: lifecycle, caller restrictions,
  manifest-declared tools, and capability needs.
- Admit System Agent, permitted external Agent, and authenticated CLI callers,
  never Apps or App-owned agents. Cross-App orchestration is system-Agent-owned.
- Provide public SDK calls without exposing internal broker details.
- AI chat retains its public options and error envelope while `cos ai chat`
  transports bounded prompt/system/tool/budget data to the authenticated broker.
  Hosted Apps neither resolve local session registries nor own provider access.
- Keep language bindings behaviorally compatible.
- Own decoder validation and JSON-RPC error codes in `wire/v1/contract.json`
  plus the versioned schemas.
- Release every language binding at the same SDK SemVer through GitHub.
- Generate, rather than hand-edit, generated bindings.

## Key Files

| Path | Role |
| --- | --- |
| `wire/` | Versioned contract and code generation |
| `wire/v1/contract.json` | Generated decoder set, stable validation errors, and JSON-RPC codes |
| `wire/v1/ask-claw-launcher.md` | Versioned secure desktop overlay launcher handshake |
| `wire/v1/manifest.schema.json` | Versioned App/MCP service, tool, access, and capability contract |
| `wire/v1/mcp_call_context.schema.json` | Gateway-authenticated caller identity, task/session correlation, and deadline |
| `rust/` | Rust public SDK |
| `rust/src/lib.rs` | Shared CLI wire/error decoder, including explicit executable selection and bounded stdin for controlled primitive business data |
| `rust/notification-presentation/`, `wire/v1/notification-presentation.md` | OS-defined rendering/preferences companion contract; no App config/UI dependency, authority store or renderer feature coupling |
| `python/` | Python public SDK |
| `node/` | Node public SDK |
| `go/` | Go public SDK |
| `python/src/claw_os_sdk/generated.py` | Generated Python wire bindings |
| `python/src/claw_os_sdk/mcp.py` | Manifest-bound MCP server, progress, and cooperative cancellation |
| `python/src/claw_os_sdk/kernel.py` | Explicit installed-binary stdin transport, inherited broker session, strict shared wire errors, deadlines and cancellation/reaping |
| `../.github/workflows/publish-sdk-release.yml` | GitHub-only multi-language SDK release |
| `../docs/app-platform.md` | Versioned App development artifact containing SDK/runtime/native library exports, never App support or private OS providers |

`cos-runtime/` is a separate internal package for bundled apps; public apps
must not depend on its policy/runtime internals.

## Dependencies

Wire schemas and `wire/v1/contract.json` are the source of truth. Core, MCP,
and every language SDK consume them.
Serialization changes stay backwards compatible unless introduced under a new
wire version.
JSON Schema integers use mathematical semantics: finite values with no
fractional component, including `1.0` and exponent notation. Type validation
runs before schema minimum/maximum checks in every generated decoder. Wire
number lexemes are preserved and evaluated as exact decimal rationals before
conversion; u64-domain Node values materialize as `bigint` when necessary.
Unrestricted payloads use `serde_json::Value` (Rust), `Decimal`/`WireDecimal` plus
`encode_wire_json` (Python), `WireDecimal`/`bigint` plus `stringifyWireJson`
(Node), and `json.Number` with `encoding/json` (Go).

All GUI bindings preserve their existing `open_agent_overlay` signatures but
delegate to the fixed `/usr/local/bin/cos-ask-claw-launcher`. The helper uses a
versioned, peer-credential-authenticated AF_UNIX protocol and exclusively owns the secure Agent UI
handoff; bindings do not invoke `cos-agent-ui`, consult `PATH`, or put hints in
argv/environment/files.

The Rust controlled-primitive stdin transport captures diagnostics by default.
Its explicit human-terminal variant only inherits an already-connected stderr
terminal; it never creates a TTY, changes identity, or bypasses the OS bootstrap
guards. Capture uses it for direct CLI compatibility, never for MCP calls.
Installed Settings uses the explicit-binary variant for `/usr/local/bin/cos`;
it does not depend on launcher PATH or a process-wide environment mutation.
Generic SDK clients still honor `CLAW_COS_BIN` and otherwise resolve `cos` on PATH.
Media Player uses `cos_call_json_async_with_binary`: it shares the strict wire
decoder and fixed binary selection, but dropping an MCP call kills/reaps the
CLI child. The OS deadline and fresh grant gate still govern undispatched work;
an already accepted playback action cannot be rolled back by cancellation.
Notifications uses `cos_call_json_async_with_stdin_binary` to keep bounded
business text off argv while retaining explicit executable selection, shared
wire errors and cancellation/reaping. It does not convey authority in stdin.
The Python equivalent is
`kernel.call_json_with_stdin_binary(binary, args, data, deadline_unix_ms=...,
check_cancelled=...)`. Callers provide the complete primitive argv and pass
the MCP context's cancellation check, not identity metadata or a new session.
It uses the existing wire decoder and never mutates PATH/environment.
Cancelled or timed-out accepted mutations are not automatically retried.

## Tests

Rust SDK unit tests mirror `rust/src/` under `rust/test/unit/`; production files
only contain cfg(test) include declarations.

Regenerate from this directory after wire changes:

```bash
cd claw-os-sdk
python3 wire/codegen.py
python3 wire/codegen.py --check
```

The generator writes the four SDK bindings plus the core and Rust SDK MCP
JSON-RPC constant modules.

Then run the affected language tests plus the repository Python suite:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q claw-os-sdk/python/src
cargo test -p claw-os-sdk
```

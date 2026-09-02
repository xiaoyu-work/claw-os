# SDK Module

## Purpose

`claw-os-sdk/` defines the public, language-neutral app/agent contract and its
Rust, Python, Node, and Go bindings.

## Responsibilities

- Maintain versioned wire types and operation/capability schemas.
- Define the MCP-first App service contract: lifecycle, caller restrictions,
  manifest-declared tools, and capability needs.
- Provide public SDK calls without exposing internal broker details.
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
| `wire/v1/mcp_call_context.schema.json` | Gateway-authenticated caller identity, lineage, depth, and deadline |
| `rust/` | Rust public SDK |
| `python/` | Python public SDK |
| `node/` | Node public SDK |
| `go/` | Go public SDK |
| `python/src/claw_os_sdk/generated.py` | Generated Python wire bindings |
| `python/src/claw_os_sdk/mcp.py` | Manifest-bound MCP server, progress, and cooperative cancellation |
| `../.github/workflows/publish-sdk-release.yml` | GitHub-only multi-language SDK release |

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

## Tests

Rust SDK unit tests mirror `rust/src/` under `rust/test/unit/`; production files
only contain cfg(test) include declarations.

Regenerate from this directory after wire changes:

```bash
cd claw-os-sdk
python3 wire/codegen.py
python3 wire/codegen.py --check
```

The generator writes the four SDK bindings plus the core and
`cos-mcp-serve` JSON-RPC constant modules.

Then run the affected language tests plus the repository Python suite:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q claw-os-sdk/python/src
cargo test -p claw-os-sdk
```

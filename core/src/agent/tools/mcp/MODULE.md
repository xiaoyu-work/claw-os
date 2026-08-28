# MCP Tools Module

## Purpose

`tools/mcp/` implements the agent's outbound MCP client, inbound MCP server,
transports, discovery, and tool-registry integration.

## Responsibilities

- Implement the validated JSON-RPC/MCP protocol subset.
- Attach configured or discovered stdio/remote servers.
- Prefix and register remote tools in the guarded registry.
- Expose local tools to external MCP clients.
- Bound frames, handshakes, requests, and optional-server failures.

## Key Files

| Path | Role |
| --- | --- |
| `protocol.rs` | MCP/JSON-RPC types and protocol version |
| `generated.rs` | JSON-RPC error codes generated from the SDK wire contract |
| `transport.rs` | Bounded transport abstraction and stdio |
| `client.rs` | Request lifecycle and reader task |
| `server.rs` | Local tools/list/tools/call server |
| `integration.rs` | Process/URL attachment and registry adapters |
| `discover.rs` | XDG agent-API sidecar discovery |

## Provenance

Presence in an XDG search path is not authority to execute. A discovered
agent-API package must be a directory carrying a `claw.provenance/v1`
envelope signed by a trusted publisher, root-owned content under an
approved system package root, or covered by an explicit developer grant.
Loose `*.json` manifests are honoured only under an approved,
root-owned package root. The manifest is read from the verified
snapshot, and the command, its arguments and any package-relative env
paths are re-verified immediately before spawn; a package may otherwise
run only a root-owned distribution interpreter, never a writable one
found earlier on `PATH`. Tool names, descriptions and results stay
untrusted model input even when the package is signed.

## Dependencies

MCP attachment is optional and must not prevent the agent from starting.
Remote tool descriptors/results remain untrusted and pass through the normal
registry, capability, and prompt-injection boundaries.
Equivalent first-party MCP failures use the generated codes owned by
`claw-os-sdk/wire/v1/contract.json`.

A stdio server is third-party code and is launched through the shared
hostile-worker sandbox in [`crate::worker`](../../../worker/MODULE.md),
never with a local `Command`. It gets a read-only system image, the
configured working directory (checked against the owner's home), no App
data directory, no network namespace of the host's, no inherited
environment, and a per-launch broker endpoint that shadows the real
broker socket and admits nothing by default. Its authority arrives per
call as transient capabilities the kernel sets on the session, so a
`tools/list` or `tools/call` result can never become authority. Keep the
`LaunchResources` on the server handle: dropping the handle is what
tears the sandbox and its endpoints down.

## Tests

```bash
cargo test -p cos agent::tools::mcp:: -- --test-threads=1
```

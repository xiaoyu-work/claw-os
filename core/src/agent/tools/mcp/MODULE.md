# MCP Tools Module

## Purpose

`tools/mcp/` implements the agent's outbound MCP client, inbound MCP server,
transports, discovery, and tool-registry integration.

## Responsibilities

- Implement the validated JSON-RPC/MCP protocol subset.
- Attach configured or discovered stdio/remote servers.
- Route attachment and calls through `claw-extension-host` for supervised
  tasks; direct CLI/web processes retain the local client path.
- Register only the fixed locally-authored `mcp_catalog` and `mcp_invoke`
  progressive-disclosure gateways. Remote names and argument-property names
  are never `llm::Tool` names or schema keys.
- Return remote names and sanitized structural schemas only inside the
  standard untrusted-data envelope, paired with opaque random invocation
  handles.
- Bind calls to a canonical digest of the sanitized descriptor set and relist
  before execution; structural drift requires a new authorized attachment.
- Expose only the external client's session-projected local tools and repeat
  the same projection check on `tools/call`.
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

## Dependencies

MCP attachment is optional and must not prevent the agent from starting.
Remote tool descriptors/results remain untrusted and pass through the normal
registry, session exposure, capability, and prompt-injection boundaries.
Handles are bound to owner, authority session, task, capability generation,
server, and descriptor digest; reconnect, guessing, drift, and cross-session
replay fail closed.
Descriptor schemas are size/depth/node bounded and retain only structural
object/property/required/item/combinator/cardinality constraints. `$ref` and
logical reference cycles fail closed.
Equivalent first-party MCP failures use the generated codes owned by
`claw-os-sdk/wire/v1/contract.json`.

## Tests

```bash
cargo test -p cos agent::tools::mcp:: -- --test-threads=1
cargo test -p cos agent::tools::registry:: -- --test-threads=1
```

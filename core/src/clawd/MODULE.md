# clawd Module

## Purpose

`clawd/` is the privileged system broker behind daemon-backed `cos` operations
and agent tasks.

## Responsibilities

- Accept authenticated Unix-socket RPC.
- Derive client/session identity and capability context.
- Dispatch privileged services and app/MCP session operations.
- Run agent task workers and expose task lifecycle RPC.
- Install audit hooks around broker-visible work.

## Key Files

| Path | Role |
| --- | --- |
| `server.rs` | Socket lifecycle, request routing, peer checks |
| `agent_client.rs` | Client RPC for agent task submit/result/cancel/status |
| `tasks.rs` | Task queue and lifecycle |
| `app_sessions.rs` | Native app and MCP session registration |
| `system_caps.rs` | System capability derivation |
| Service modules | One privileged capability provider per domain |

## Dependencies

The broker consumes capability definitions and service providers. Callers use
RPC clients rather than importing server internals. Never trust request fields
for identity or authority; derive them from the connection/session boundary.

## Tests

```bash
cargo test -p cos clawd:: -- --test-threads=1
```

For a service change, include malformed input, exact scope, broker error, and
successful provider-path coverage.

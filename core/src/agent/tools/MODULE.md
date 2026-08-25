# Agent Tools Module

## Purpose

`tools/` defines every model-visible tool and the guarded registry through
which tool calls are exposed and executed.

## Responsibilities

- Register built-in, `cos` proxy, app, memory, browser, and MCP tools.
- Convert tool schemas into LLM-facing definitions.
- Apply guardrails and session/capability context.
- Keep untrusted tool output inside explicit model-data boundaries.

## Key Files

| Path | Role |
| --- | --- |
| `registry.rs` | Tool registration, filtering, lookup |
| `guardrails.rs` | Tool exposure/dispatch policy |
| `cos_proxy/` | Structured `cos` primitive tools |
| `cos_apps.rs`, `cos_apps_session.rs` | App tool discovery/session calls |
| `mcp/` | MCP attachment and proxy tools |
| `memory.rs`, `recall.rs` | Agent memory tools |

## Dependencies

Runtime dispatch depends on the registry, never on concrete tools directly.
Tools consume stable service/capability definitions. Model output and external
tool results are untrusted; authority comes only from session and capability
context.

## Tests

```bash
cargo test -p cos agent::tools:: -- --test-threads=1
```

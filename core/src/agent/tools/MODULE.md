# Agent Tools Module

## Purpose

`tools/` defines every model-visible tool and the guarded registry through
which tool calls are exposed and executed.

## Responsibilities

- Register built-in, `cos` proxy, progressive app, memory, browser, and MCP tools.
- Let an attended local system Agent initiate trusted account authorization
  without exposing OAuth tokens or client secrets to the model.
- Convert tool schemas into LLM-facing definitions.
- Apply guardrails and session/capability context.
- Declare whether consent is enforced by an exact capability gate or
  by the legacy tool-name compatibility filter.
- Keep untrusted tool output inside explicit model-data boundaries.

## Key Files

| Path | Role |
| --- | --- |
| `registry.rs` | Tool registration, filtering, lookup |
| `guardrails.rs` | Tool exposure/dispatch policy |
| `cos_proxy/` | Structured `cos` primitive tools |
| `cos_proxy/oauth_login.rs` | Agent-initiated trusted OAuth browser flow |
| `cos_apps.rs`, `cos_apps_session.rs` | Compact app catalog/run gateways and active session calls |
| `mcp/` | MCP attachment and proxy tools |
| `memory.rs`, `recall.rs` | Agent memory tools |

## Dependencies

Runtime dispatch depends on the registry, never on concrete tools directly.
Tools consume stable service/capability definitions. Model output and external
tool results are untrusted; authority comes only from session and capability
context. `auto_deny_tools` may block any tool early, but
`dangerous_tools`/`auto_approve_tools` never grant capability authority. Only
proxies whose complete command surface has an exact mapping declare a
capability-aware boundary; mixed or incomplete proxies stay on the legacy
filter. Only
proxies whose complete command surface has an exact mapping declare a
capability-aware boundary; mixed or incomplete proxies stay on the legacy
filter.

## Tests

```bash
cargo test -p cos agent::tools:: -- --test-threads=1
```

# Agent Tools Module

## Purpose

`tools/` defines every model-visible tool and the guarded registry through
which tool calls are exposed and executed.

## Responsibilities

- Register built-in, `cos` proxy, progressive app, memory, browser, and MCP tools.
- Use host-backed proxy tools in supervised tasks so dynamic App/MCP code
  never executes in the worker process.
- Cache immutable name/description/schema descriptors separately from
  per-request visibility decisions.
- Admit remote MCP descriptors only after safe-name normalization, recursive
  schema annotation removal, strict structural-schema validation, and
  collision-free registration. Remote prose never enters descriptor cache.
- Project descriptors through trusted session owner, source, attendance,
  capabilities, host transports, enabled extensions, and guardrails.
- Replace oversized permitted extension catalogs with fixed
  `cos_tool_search`, `cos_tool_describe`, and `cos_tool_call` schemas while
  keeping core and small catalogs direct.
- Let an attended local system Agent initiate trusted account authorization
  without exposing OAuth tokens or client secrets to the model.
- Convert tool schemas into LLM-facing definitions.
- Apply guardrails and session/capability context.
- Require `memory.read:self:agent` versus `memory.write:self:agent` after
  validating each `cos_memory` or `cos_todo` command and resource.
- Scope conversation recall to `self:<session>` unless the caller holds the
  system-Agent memory scope; app-memory queries require `self:<app>` before
  source-filtered rows are returned.
- Advertise media tools only when a configured provider has a compatible
  exact name scope, then recheck that provider immediately before invocation;
  STT independently enforces its exact `fs.read` path scope.
- Declare whether consent is enforced by an exact capability gate or
  by the legacy tool-name compatibility filter.
- Keep untrusted tool output inside explicit model-data boundaries.

## Key Files

| Path | Role |
| --- | --- |
| `registry.rs` | Immutable descriptor/tool registration and projected lookup |
| `exposure.rs` | Typed session facts, availability requirements, projection decisions |
| `progressive.rs` | Schema budgeting plus stable extension search/describe/call bridges |
| `guardrails.rs` | Tool exposure/dispatch policy |
| `cos_proxy/` | Structured `cos` primitive tools |
| `cos_proxy/oauth_login.rs` | Agent-initiated trusted OAuth browser flow |
| `cos_apps.rs`, `cos_apps_session.rs` | Compact app catalog/run gateways and active session calls |
| `mcp/` | MCP attachment and proxy tools |
| `memory.rs`, `recall.rs` | Agent memory tools |

## Dependencies

Runtime dispatch depends on the registry plus one `ToolExposureContext`, never
on concrete tools directly. Projection is rebuilt per request and repeated at
dispatch; only immutable descriptors may be cached. Tools consume stable
service/capability definitions and still perform exact argument-derived checks.
Model output, client fields, process environment, and external tool results are
untrusted; authority comes only from authenticated session/runtime facts.
Hosted results are wrapped as untrusted model data before they enter the
trajectory. Hosted descriptor calls carry the canonical sanitized descriptor
set digest; relist drift fails closed for the rest of that attachment.

Bridge calls are resolved to the original tool identity before hooks,
parallelism, approval, and execution. Direct calls to deferred names are
rejected, and attachment liveness is rechecked immediately before execution.
Search returns only length- and count-bounded metadata under a hard serialized
response budget; exact schemas are returned only by `cos_tool_describe`.

`auto_deny_tools` may block any tool early, but
`dangerous_tools`/`auto_approve_tools` never grant capability authority. Only
proxies whose complete command surface has an exact mapping declare a
capability-aware boundary; mixed or incomplete proxies, including `cos_proc`,
stay on the legacy filter.

## Tests

```bash
cargo test -p cos agent::tools:: -- --test-threads=1
```

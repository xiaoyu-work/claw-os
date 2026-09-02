# Agent Tools Module

## Purpose

`tools/` defines every model-visible tool and the guarded registry through
which tool calls are exposed and executed.

## Responsibilities

- Register built-in, `cos` proxy, progressive App, memory, browser, and MCP tools.
- Use host-backed proxy tools in supervised tasks so dynamic App/MCP code
  never executes in the worker process.
- Cache immutable name/description/schema descriptors separately from
  per-request visibility decisions.
- Admit remote MCP descriptors only after safe-name normalization, recursive
  schema annotation removal, strict structural-schema validation, and
  collision-free registration. Remote prose never enters descriptor cache.
- Project descriptors through trusted session owner, source, attendance,
  capabilities, host transports, enabled extensions, and guardrails.
- Replace oversized permitted extension catalogs with fixed bounded gateways
  while keeping core and small App catalogs direct.
- Let an attended local system Agent initiate trusted account authorization
  without exposing OAuth tokens or client secrets to the model.
- Convert tool schemas into LLM-facing definitions.
- Keep core schemas direct while progressively disclosing App tools through a
  fixed search/describe/call bridge; MCP uses a separate fixed
  catalog/invoke gateway.
- Apply guardrails and session/capability context.
- Require `memory.read:self:agent` versus `memory.write:self:agent` after
  validating each `cos_memory` or `cos_todo` command and resource.
- Scope conversation recall to `self:<session>` unless the caller holds the
  system-Agent memory scope; app-memory queries require `self:<app>` before
  source-filtered rows are returned.
- Advertise media tools only when a configured provider has a compatible exact
  name scope, then recheck that provider immediately before invocation; STT
  independently enforces its exact `fs.read` path scope.
- Declare whether consent is enforced by an exact capability gate or by the
  legacy tool-name compatibility filter.
- Default-deny Agent-extension proposals unless a tool declares a cooperative,
  exact input-to-capability policy; higher-order, credential, provider, MCP,
  process, shell, and legacy proxy tools remain non-proposable.
- Keep untrusted tool output inside explicit model-data boundaries.

## Key Files

| Path | Role |
| --- | --- |
| `registry.rs` | Tool registration, filtering, lookup, and explicit registry resources/paths |
| `progressive.rs` | Deferred-tool classification, compact catalog, bridge schemas, and envelope validation |
| `guardrails.rs` | Tool exposure/dispatch policy |
| `cos_help.rs` | Read-only progressive discovery over the shared public `cos` command tree |
| `cos_proxy/` | Structured `cos` primitive tools |
| `cos_proxy/oauth_login.rs` | Agent-initiated trusted OAuth browser flow |
| `cos_apps.rs`, `cos_apps_session.rs` | Compact app catalog/run gateways and active session calls |
| `mcp/` | MCP attachment and proxy tools |
| `memory.rs`, `recall.rs` | Agent memory tools |

## Dependencies

Runtime dispatch depends on the registry plus one trusted
`ToolExposureContext`, never on concrete tools directly. Composition resolves
`RegistryPaths`, optional memory/semantic stores, App session manifests, and
immutable configuration into `RegistryDeps`; assembling
`default_registry_with_deps(&deps)` performs no environment reads or store
opens. Registry paths also preserve the system Skill trust origin, exact App
root, generic App catalog/run root, and one notes store shared by prompt reads,
`cos_memory`, and curation. Deprecated no-argument registry/media constructors
remain only as compatibility composition wrappers. Projection is rebuilt per
request and repeated at dispatch; only immutable descriptors may be cached.
Tools consume stable service/capability definitions and still perform exact
argument-derived checks. Model output, client fields, process environment, and
external tool results are untrusted; authority comes only from authenticated
session/runtime facts. Hosted results are wrapped as untrusted model data.

Bridge calls are resolved to the original tool identity before hooks,
parallelism, approval, and execution. Direct calls to deferred names are
rejected, and attachment liveness is rechecked immediately before execution.
Search returns only length- and count-bounded metadata under a hard serialized
response budget; exact schemas are returned only by the describe gateway.
`auto_deny_tools` may block any tool early, but
`dangerous_tools`/`auto_approve_tools` never grant capability authority.

Agent-extension action preparation is a separate registry path. It binds the
authenticated manifest policy, canonical input, exact capability, tool,
catalog generation, event, and operation digest before any reference is
consumed or approval is requested.

## Tests

```bash
cargo test -p cos agent::tools:: -- --test-threads=1
```

# Agent Tools Module

## Purpose

`tools/` defines every model-visible tool and the guarded registry through
which tool calls are exposed and executed.

## Responsibilities

- Register built-in, `cos` proxy, progressive App, memory, browser, and MCP tools.
- Let an attended local system Agent initiate trusted account authorization
  without exposing OAuth tokens or client secrets to the model.
- Convert tool schemas into LLM-facing definitions.
- Keep core schemas direct while progressively disclosing App tools through a
  fixed search/describe/call bridge; MCP uses a separate fixed
  catalog/invoke gateway.
- Apply guardrails and session/capability context.
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

Runtime dispatch depends on the registry, never on concrete tools directly.
Composition resolves `RegistryPaths`, optional memory/semantic stores, App
session manifests, and immutable configuration into `RegistryDeps`; assembling
`default_registry_with_deps(&deps)` performs no environment reads or store opens.
Registry paths also preserve the system Skill trust origin, exact App root,
generic App catalog/run root, and one notes store shared by prompt reads,
`cos_memory`, and curation. Deprecated no-argument registry/media constructors
remain only as compatibility composition wrappers. Tools
consume stable service/capability definitions. Model output and external tool
results are untrusted; authority comes only from session and capability
context. A bridge call is resolved before hooks, approval, and parallel
planning; synthetic bridge names are never registered as executable tools.

## Tests

```bash
cargo test -p cos agent::tools:: -- --test-threads=1
```

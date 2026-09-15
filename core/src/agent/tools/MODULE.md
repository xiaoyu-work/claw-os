# Agent Tools Module

## Purpose

`tools/` defines every model-visible tool and the guarded registry through
which tool calls are exposed and executed.

## Responsibilities

- Register built-in, `cos` proxy, authenticated MCP App, memory, browser, and
  external MCP tools.
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
- Keep core schemas direct while progressively disclosing authenticated MCP
  App tools through a fixed search/describe/call bridge; external MCP uses a separate fixed
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
| `cos_apps_session.rs` | Authenticated MCP App tool registration, task-Host relay, reusable/single-call placement, and calls through the daemon service Host |
| `mcp/` | MCP attachment and proxy tools |
| `cos_proxy/memory.rs`, `cos_proxy/recall.rs` | Model-selected note/history search and versioned source reads |
| `cos_proxy/recall_semantic.rs`, `cos_proxy/app_memory.rs` | Semantic/App evidence, bounded coverage and original-source handles |

The broker and Host share `classify_app_call`: editor filesystem operations
use controlled providers in a resource-free reusable worker, not file mounts.
This placement rule does not add or relax authority.

## Dependencies

Memory tools expose scoped search and revision-bound source pages. Their
results retain the source's trust label and disclose partial coverage; scores
are retrieval relevance, not truth. The model chooses whether to refine a
query, expand a source or inspect current authorized App/OS state, and reports
unresolved gaps at the existing execution limits.
Exact and semantic session recall both check the canonical active-replay
projection; an excluded or missing source row is never returned through a
stale FTS or embedding hit.

Runtime dispatch depends on the registry plus one trusted
`ToolExposureContext`, never on concrete tools directly. Composition resolves
`RegistryPaths`, optional memory/semantic stores, App session manifests, and
immutable configuration into `RegistryDeps`; assembling
`default_registry_with_deps(&deps)` performs no environment reads or store
opens. Registry paths also preserve the system Skill trust origin, exact App
root, and one notes store shared by prompt reads,
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

Memory tools return source identity, revision, partial-page and candidate
coverage information. Follow-up pages require the source revision. Instructions
tell the main model to keep retrieving/refining when evidence is insufficient
or unreliable, to verify current facts at their App/OS source, and to report
remaining uncertainty if permissions or execution budgets prevent resolution.
No pre-model keyword router or mandatory extra planning call selects memories.

## Activity capability constraints

For brokered Apps, the invoke preflight and session-tool wrapper do not spend
consent locally. Root registration/call planning settles ordinary missing
permissions and Activity-required confirmations as one complete set. Local
App execution retains its existing preflight. Denied calls never fall back
to a local or unverified launcher; all execution still crosses the ordinary
App boundary.

## Activity receipts

The MCP App Mesh is the sole model-visible App invocation path. Root captures
Activity-associated task results in the App-service manager before this
module trust-fences them. Schema inspection creates no receipt. Recording
failures preserve the original result and provide only a recording retry;
they never repeat App execution or acquire authority. `session:<tool>` names
select MCP declarations, never a same-named ordinary operation. MCP error
flags remain reported errors, and transport uncertainty remains indeterminate.
Explicit CLI operation capture stays outside the model's App tool projection.

## Tests

```bash
cargo test -p cos agent::tools:: -- --test-threads=1
```

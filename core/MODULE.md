# Core Module

## Purpose

`core/` builds the `cos` CLI/library, the `clawd` system broker, the
unprivileged `claw-agentd` agent worker, the task-owned
`claw-extension-host`, and small privileged helper binaries.
It owns system authority, capability enforcement, agent orchestration,
persistence, and structured primitive dispatch.

## Responsibilities

- Parse and route `cos` commands.
- Broker privileged and session-scoped operations through `clawd`.
- Run the multi-turn agent, tool registry, memory, and provider integrations.
- Discover app manifests and bridge bundled app execution.
- Verify signed extension packages and run selected Agent observers through
  the out-of-process ABI.
- Enforce capability scopes and write audit/session records.
- Refuse to install, activate or run a Claw OS release older than the one this
  machine has already accepted.

## Key Files

| Path | Role |
| --- | --- |
| `src/main.rs` | `cos` process entry and output format selection |
| `CHANGELOG.md` | Versioned core API migrations and compatibility transitions |
| `src/router.rs` | Top-level command and hidden bridge dispatch |
| `src/bin/clawd.rs` | System daemon entry |
| `src/bin/claw-agentd.rs` | Unprivileged agent worker entry |
| `src/bin/claw-mail-ai-host.rs` | Trusted Thunderbird launcher for the canonical `/usr/lib/cos/apps/mail-ai` implementation |
| `src/bin/claw-security-floor.rs` | Update downgrade-protection verifier used by maintainer scripts |
| `src/update/` | Signed release manifest, monotonic security floor, recovery authorizations, runtime gates |
| `src/clawd/server.rs` | IPC broker, identity checks, RPC dispatch, audit hook |
| `src/agentd/` | Broker/runtime process split: privilege drop, job grants, worker supervision, consent mediation |
| `src/extension_host/` | Isolated App/MCP process host, task-bound control channel, route-filtered broker proxy, cleanup |
| `src/agent_extensions/` | Verified manifest registry, event fanout, capability references, and proposed-action mediation |
| `src/provenance/` | Compiled-root signature verification and immutable package snapshots |
| `src/agent/` | Agent CLI, runtime, tools, LLM providers, memory, and web UI |
| `src/caps/` | Capability catalog, scopes, manifests, and enforcement |
| `src/worker/` | Shared hostile-worker sandbox: launch policy, Linux provider, per-launch brokers |
| `src/apps.rs` | `app.json` discovery and side-effect-free schema generation |
| `src/audit.rs` | Hash-chained audit persistence |
| `src/audit_policy.rs` | Per-command/per-tool allowlist every durable audit projection applies |
| `src/session/` | Session storage and lifecycle |

## Dependencies

Entry points and orchestration depend on stable service/capability definitions.
Providers implement those definitions; consumers must not import around them.
`clawd` is the privileged boundary, and it does not run the model/tool loop:
that executes in `claw-agentd` — see [`src/agentd/MODULE.md`](src/agentd/MODULE.md).
App code and model output are untrusted inputs at this layer.

Read [`src/agent/MODULE.md`](src/agent/MODULE.md) before changing agent code.
Project-wide rules are in [`../ARCHITECTURE.md`](../ARCHITECTURE.md).

## Tests

App contract and worker integration cases use
`test/support/app_sources.rs` to select source by the OS App lock. Prepare
migrated product inputs from the repository root with
`python3 scripts/app_sources.py` before running these cases. Missing pinned
inputs fail with an actionable diagnostic; tests do not download them.
The helper resolves the lock's distinct `products` and optional `capabilities`
roots, requires matching source-kind metadata and exact identity/layout, and
never substitutes local `apps/doc`, `apps/db`, `apps/kv` or `apps/net` for
migrated clients.
`tests/app_source_fixtures.rs` covers both kinds, missing/duplicate/escaping
sources and the published Doc/DB/KV manifests, including exact MCP/CLI arguments,
defaults and separate key/database/whole-store scopes. Signed fixtures in
`tests/extension_provenance_process.rs` bind the same sources to worker policy;
DB and KV retain independent owner/App partitions, without neighbouring App or
Agent-memory mounts. `test/support/app_stage.rs` reuses the declared staging CLI,
checks the prepared cache's exact Git revision/cleanliness without downloads,
and supplies the OS-owned shared Python library separately from App payloads.
Signed fixtures and the real KV session tests bind the manifest-selected
entrypoint rather than assuming private implementation filenames.
The KV planner regression verifies distinct key read/write/delete grants and
fixed whole-store read for list/dump; named-key unions cannot authorize enumeration.
These pinned integration inputs are not production core build dependencies.

```bash
# Narrow test or module
cargo test -p cos <test-filter> -- --test-threads=1

# Immutable product/capability fixture resolution.
cargo test -p cos --test app_source_fixtures -- --test-threads=1

# Full core suite
(cd core && cargo test -- --test-threads=1)

# Update downgrade protection, including the dpkg ordering cross-check
cargo test -p cos --test security_floor_process -- --test-threads=1

# CI lint
(cd core && cargo clippy -- -D warnings)
```

Many tests mutate process-global environment variables; combined runs stay
single-threaded.

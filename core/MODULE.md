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
| `src/bin/claw-security-floor.rs` | Update downgrade-protection verifier used by maintainer scripts |
| `src/update/` | Signed release manifest, monotonic security floor, recovery authorizations, runtime gates |
| `src/clawd/server.rs` | IPC broker, identity checks, RPC dispatch, audit hook |
| `src/agentd/` | Broker/runtime process split: privilege drop, job grants, worker supervision, consent mediation |
| `src/extension_host/` | Isolated App/MCP process host, task-bound control channel, route-filtered broker proxy, cleanup |
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

```bash
# Narrow test or module
cargo test -p cos <test-filter> -- --test-threads=1

# Full core suite
(cd core && cargo test -- --test-threads=1)

# Update downgrade protection, including the dpkg ordering cross-check
cargo test -p cos --test security_floor_process -- --test-threads=1

# CI lint
(cd core && cargo clippy -- -D warnings)
```

Many tests mutate process-global environment variables; combined runs stay
single-threaded.

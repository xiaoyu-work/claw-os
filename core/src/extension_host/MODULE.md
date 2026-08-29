# Extension Host Module

## Purpose

`extension_host/` keeps dynamic App and MCP code outside both privileged
`clawd` and the model/tool worker. `clawd` spawns one
`claw-extension-host` process per task as the task owner; `claw-agentd`
uses a separate task-bound control socket to attach, call, cancel, and detach
extensions.

## Responsibilities

- Spawn the host with no supplementary groups, `NoNewPrivs`, `0077` umask,
  descriptor and environment allowlists, its own session/process group, finite
  rlimits, and an optional cgroup/namespace layer.
- Bind the worker control channel to owner, task, session, worker pid/start
  time, host pid/start time, nonce, protocol version, and lease deadline.
- Expose a second broker-owned Unix socket only to the host process and its
  descendants. The host may use only App/MCP lifecycle routes; descendants may
  use only session/peer-session provider routes for their nearest registered
  App/MCP session.
- Run one-shot Apps, stateful App MCP servers, and configured MCP servers;
  never run their code in `clawd` or `claw-agentd`.
- Bound frames, concurrent calls, startup/call timeouts, and replay history.
- Tear down the host cgroup/process tree and every child session on task
  completion, cancellation, timeout, crash, or worker loss.
- Treat descriptors and results returned by hosted code as untrusted.

## Key Files

| Path | Role |
| --- | --- |
| `protocol.rs` | Versioned worker-control contract and signed binding fields |
| `spawn.rs` | Privilege drop, fd/env/resource isolation, optional cgroup/namespaces, descendant cleanup |
| `client.rs` | `claw-agentd` client used by App and MCP registry adapters |
| `host.rs` | Host process, control admission, App/MCP lifecycle and cancellation |
| `broker.rs` | Per-task broker proxy socket, SCM credential verification, route/session allowlists |
| `../bin/claw-extension-host.rs` | Installed host executable |

## Authority and Data Flow

```text
clawd supervisor
  -> spawn claw-agentd
  -> bind private extension broker socket
  -> spawn claw-extension-host as task owner
  -> register exact host pid/start-time as an extension-host session
  -> sign host pid/start-time + socket paths + nonce into the worker grant

claw-agentd registry call
  -> versioned control request to exact host pid
  -> host starts/calls App or MCP child
  -> child policy check reads its root-maintained session row
  -> child privileged request uses COS_EXTENSION_BROKER_SOCKET
  -> clawd verifies per-message credentials and nearest child session
  -> normal route registry, capability authority, provider check and audit
```

The private proxy is not `/run/cos/clawd.sock`. A same-uid sibling can discover
or connect to its pathname, but it is rejected unless its kernel pid/start-time
is the exact host or a descendant bound to the nearest App/MCP session.
Lifecycle routes are host-only; provider routes are child-only; task,
scheduler, permission-decision, admin, and App-session routes are never
available to an extension child.

## Tests

```bash
cargo test -p cos extension_host -- --test-threads=1
cargo test -p cos --test extension_host_boundary -- --test-threads=1
cargo test -p cos --test agentd_process_boundary -- --test-threads=1
bash packaging/deb/tests/test-agentd-packaging.sh
```

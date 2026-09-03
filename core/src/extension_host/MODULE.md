# Extension Host Module

## Purpose

`extension_host/` keeps dynamic App and MCP code outside both privileged
`clawd` and the model/tool worker. Each `claw-extension-host` has an explicit
purpose. A short-lived task Host is controlled by the exact `claw-agentd`
worker and relays App calls; an owner/App-scoped service Host is controlled by
the exact root `clawd` process and executes persistent App MCP services. Both
use package-reserved locked accounts with the dedicated `cos-extension`
primary gid.

## Responsibilities

- Spawn the host with an exclusive package-reserved uid, dedicated non-broker gid, no supplementary
  groups, `NoNewPrivs`, `0077` umask,
  descriptor and environment allowlists, its own session/process group, finite
  rlimits, mandatory verified cgroup-v2 containment, and optional IPC/UTS
  namespaces.
- Bind every control channel to Host purpose, owner, controller uid/gid plus
  pid/start time, host pid/start time, task or service identity, package when
  applicable, nonce, protocol version, and lease deadline.
- Expose a second broker-owned Unix socket only to the host process and its
  descendants. The host may use only App/MCP lifecycle routes; descendants may
  use only session/peer-session provider routes for their nearest registered
  App/MCP session.
- Let task Hosts run one-shot Apps/configured MCP and relay typed App Mesh
  calls, but never execute those relayed calls.
- Let service Hosts execute only daemon-authorized calls for their exact bound
  App/package and warm `always-on` MCP children.
- Roll task Host leases forward only after authenticated worker heartbeats;
  service Host leases remain fixed daemon-owned lifetimes.
- Launch every dynamic child through the trusted isolation wrapper in its own
  PID and mount namespaces with private procfs. The child sees an empty
  filesystem root, a verified read-only runtime/snapshot allowlist, and
  private writable state/tmpfs only.
- Bound frames, concurrent calls, startup/call timeouts, and replay history.
- Return only sanitized MCP descriptors and require every hosted MCP call to
  carry the exact canonical descriptor-set digest held by that host session.
  A relist mismatch blocks the call rather than substituting a new schema.
- Tear down a task Host on task completion, cancellation, timeout, crash, or
  worker loss. Service Hosts instead follow signed manifest lifecycle, package
  freshness, owner/App liveness, lease, idle, capacity, and restart policy.
- Treat descriptors and results returned by hosted code as untrusted.
- Keep every setup parent root-owned and pinned by directory descriptor.
  Create/open/remove children with `*at`/`openat2` calls; transfer only the
  final control directory after the broker socket endpoint is verified.

## Key Files

| Path | Role |
| --- | --- |
| `protocol.rs` | Versioned purpose/controller control contract and signed binding fields |
| `identity.rs` | Exact package-created account/manifest/subid validation, disjoint task/service leasing, and safe reuse |
| `spawn.rs` | Privilege drop, fd/env/resource isolation, mandatory cgroup-v2 containment, optional namespaces, verified descendant cleanup |
| `child_isolation.rs` | Per-App/MCP bubblewrap PID/proc/empty-root isolation and verified snapshots |
| `client.rs` | Purpose-bound controller client used by task workers and `clawd` |
| `host.rs` | Host process, purpose-specific control admission, App/MCP lifecycle and cancellation |
| `broker.rs` | Per-Host broker proxy socket, SCM credential verification, purpose/route/session allowlists |
| `../bin/claw-extension-host.rs` | Installed host executable |

## Authority and Data Flow

```text
clawd task supervisor
  -> lease task identity and spawn task Host controlled by exact worker
  -> sign purpose + controller/Host identities into the worker grant
  -> heartbeat renews private broker lease -> typed deadline ack to worker

claw-agentd App call
  -> versioned control request to exact task Host
  -> task Host private broker -> clawd app_service.call
  -> daemon re-authorizes and selects owner/App service Host
  -> caller-deadline, single-use action and mount-identity authorization
  -> service Host validates daemon-canonical arguments without re-resolving paths
  -> service Host starts/calls App MCP child
  -> child policy check reads its root-maintained session row
  -> child privileged request uses COS_EXTENSION_BROKER_SOCKET
  -> clawd verifies per-message credentials and nearest child session
  -> normal route registry, capability authority, provider check and audit
```

The private proxy is not `/run/cos/clawd.sock`. It belongs to the leased
extension uid and rejects every other uid before checking whether the kernel
pid/start-time is the exact host or a descendant bound to the nearest App/MCP
session.
Lifecycle routes are host-only; provider routes are child-only; task,
scheduler, permission-decision, admin, and App-session routes are never
available to an extension child.

The task binding signed at bootstrap remains immutable. The supervisor stores
the rolling private-broker deadline separately and returns each renewal over
the authenticated worker channel; the worker mirrors it in its installed
client for request and App-context validation. Exact-task and maximum-horizon
checks reject forged or stale acknowledgements, and the broker's own rolling
lease is still checked before every routed effect. App service Hosts do not
accept task heartbeat renewal.

`HostPaths` never performs privileged metadata changes through a pathname
under user control. It upgrades a legacy task-owned per-user directory by
pinning it without following links, taking ownership through the descriptor,
and recursively unlinking stale entries with `unlinkat`; symlinks and
hardlinks are removed, never followed. The broker socket lives in a
root-owned task directory. Its pathname must be absent, single-link, and a
Unix socket whose device/inode remains stable and whose listener inode maps to
that exact path in `/proc/net/unix` before the control directory is handed to
the unprivileged host.

The cgroup is created and its CPU, memory, process, OOM-group, membership, and
`cgroup.kill` controls are verified before the host is returned to the
supervisor. The host enters it in `pre_exec`, before privilege drop and
`exec`. The same closure must create a private mount namespace and task-private
tmpfs mounts for `/tmp`, `/var/tmp`, `/dev/shm`, and `/run/lock`; the remaining
mount tree is read-only except for the pinned task directory. Cleanup never
reconstructs ancestry from `/proc`: it revokes the proxy, writes
`cgroup.kill`, waits for recursive `populated 0` plus an empty `cgroup.procs`,
removes the cgroup, unmounts private filesystems, recursively removes the
descriptor-pinned task tree without crossing mounts, and then revokes ACLs.
Any failure is terminal and audited as `cleanup-failed`.

The uid pool is the fixed package-created account set `cos-ext-00..63`
(`61000..61063`), outside systemd DynamicUser. Identities `00..55` are reserved
for task Hosts and `56..63` for App service Hosts, so persistent services
cannot exhaust task execution. Concurrent Hosts never share an extension uid.
A uid is released only after verified cgroup
emptiness, private-mount teardown, task-state removal, and routed-registry ACL
revocation. A durable quarantine marker makes failures survive broker restart;
startup recovery keeps the uid unavailable until residue is safely purged.
The proxy preserves task-owner
authority as a separate principal field while recording the actual execution
uid. Host/child seccomp denies ptrace, process-vm access, `kcmp`, and
`pidfd_getfd`. Each final App/MCP child has a private PID namespace and procfs,
so same-UID sibling processes cannot enumerate, signal, or read one another.
The empty child root exposes `/usr` as verified read-only runtime content,
copies explicitly authorized non-system code into a read-only snapshot, binds
only the exact broker/session endpoints, and supplies private home/data/cache/
log/tmp paths. Pre-pivot descriptors and cwd do not survive.

For a call-scoped App, that final sandbox starts with only the trusted
`claw-app-runner` blocked on a private stdin gate. Package code cannot execute
until the service Host has bound the runner session and atomically consumed
the daemon's single-use authorization. Releasing the gate then replaces the
runner with the signed package entrypoint. The token is only a sequencing
secret on the parent-owned pipe; transient capabilities still come solely
from daemon ticket consumption.

The supervisor sends a blocked PREPARE, persists the queue's execution COMMIT,
and only then releases the worker. Failures proven pre-COMMIT may retry;
commit persistence/delivery ambiguity, unexpected host/worker exit, EOF, or
timeout after COMMIT is terminal indeterminate rather than a queue replay.

## Tests

```bash
cargo test -p cos extension_host -- --test-threads=1
cargo test -p cos --test extension_host_boundary -- --test-threads=1
cargo test -p cos --test agentd_process_boundary -- --test-threads=1
bash packaging/deb/tests/test-agentd-packaging.sh
```

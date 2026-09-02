# Extension Host Module

## Purpose

`extension_host/` keeps dynamic App and MCP code outside both privileged
`clawd` and the model/tool worker. `clawd` leases a package-reserved locked account and spawns
`claw-extension-host` with that uid plus the dedicated `cos-extension` primary
gid. Legacy extensions use a task-controlled Host; MCP-first App services use
one daemon-controlled Host per owner and separate isolated children per App.

## Responsibilities

- Spawn the host with an exclusive package-reserved uid, dedicated non-broker gid, no supplementary
  groups, `NoNewPrivs`, `0077` umask,
  descriptor and environment allowlists, its own session/process group, finite
  rlimits, mandatory verified cgroup-v2 containment, and optional IPC/UTS
  namespaces.
- Bind control to one explicit mode: exact task worker or exact root daemon
  pid/start time. A binding cannot cross modes or owners.
- Expose a second broker-owned Unix socket only to the host process and its
  descendants. The host may use only App/MCP lifecycle routes; descendants may
  use only session/peer-session provider routes for their nearest registered
  App/MCP session.
- Admit package-backed App/MCP session registration only from the authenticated
  Host execution identity. Bind the exact package reference and child process
  before provider authority becomes live.
- Run one-shot Apps, stateful App MCP servers, and configured MCP servers;
  never run their code in `clawd` or `claw-agentd`.
- Launch every dynamic child through the trusted isolation wrapper in its own
  PID and mount namespaces with private procfs. The child sees an empty
  filesystem root, a verified read-only runtime/snapshot allowlist, and
  private writable state/tmpfs only.
- Bound frames, concurrent calls, startup/call timeouts, and replay history.
- Return only sanitized MCP descriptors and require every hosted MCP call to
  carry the exact canonical descriptor-set digest held by that host session.
  A relist mismatch blocks the call rather than substituting a new schema.
- Reverify owner-scoped package trust from the root-owned routed trust
  projection and ask `clawd` to refresh/check the exact package immediately
  before hosted MCP attach and dispatch.
- Require every persistent MCP-first call to present a one-use daemon grant
  bound to Host pid/start time, owner, task/session, exact App/tool/effective
  arguments, verified manifest digest, call id, capability generation, and
  deadline. Its dedicated audience is not valid for other App lifecycle
  routes.
- Reconcile manifest lifecycle under the same per-App lock used for calls:
  idle-close `lazy`, health-check/back off `always-on`, and bind
  `while-app-running` to a live root-owned GUI session.
- Tear down task Hosts with their task and persistent Hosts on owner idle,
  daemon shutdown, or failed containment.
- Treat descriptors and results returned by hosted code as untrusted.
- Keep every setup parent root-owned and pinned by directory descriptor.
  Create/open/remove children with `*at`/`openat2` calls; transfer only the
  final control directory after the broker socket endpoint is verified.

## Key Files

| Path | Role |
| --- | --- |
| `protocol.rs` | Versioned worker-control contract and signed binding fields |
| `identity.rs` | Exact package-created account/manifest/subid validation, exclusive leasing, and safe reuse |
| `spawn.rs` | Privilege drop, fd/env/resource isolation, mandatory cgroup-v2 containment, optional namespaces, verified descendant cleanup |
| `child_isolation.rs` | Per-App/MCP bubblewrap PID/proc/empty-root isolation and verified snapshots |
| `client.rs` | `claw-agentd` client used by App and MCP registry adapters |
| `host.rs` | Host process, control admission, App/MCP lifecycle and cancellation |
| `broker.rs` | Per-task broker proxy socket, SCM credential verification, route/session allowlists |
| `../clawd/app_host.rs` | Per-owner persistent Host manager and one-use Gateway dispatch |
| `../bin/claw-extension-host.rs` | Installed host executable |

## Authority and Data Flow

```text
clawd supervisor
  -> spawn claw-agentd and sign its task/channel lease
  -> start or reuse one daemon-controlled owner Host
  -> register exact Host pid/start-time and private broker

claw-agentd MCP-first call
  -> typed private-channel request with App/tool/arguments only
  -> clawd derives caller context and target authority from the task lease
  -> one-use Host-bound dispatch grant
  -> versioned control request from exact clawd pid
  -> host starts/calls App or MCP child
  -> child policy check reads its root-maintained session row
  -> child privileged request uses COS_EXTENSION_BROKER_SOCKET
  -> clawd verifies per-message credentials, nearest child session, and live
     package/runtime binding
  -> normal route registry, capability authority, provider check and audit
```

The private proxy is not `/run/cos/clawd.sock`. It belongs to the leased
extension uid and rejects every other uid before checking whether the kernel
pid/start-time is the exact host or a descendant bound to the nearest App/MCP
session.
Lifecycle routes are host-only; provider routes are child-only; task,
scheduler, permission-decision, admin, and App-session routes are never
available to an extension child.

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
(`61000..61063`), outside systemd DynamicUser. Concurrent task Hosts never
share an extension uid. One persistent owner Host intentionally spans that
owner's Agent tasks, while each App child retains separate PID/mount/network
namespaces. A uid is released only after verified cgroup
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

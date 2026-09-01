# Dynamic Extension Host Isolation

Claw OS runs dynamic App and MCP code in `claw-extension-host`, not in
privileged `clawd` and not in the `claw-agentd` model/tool worker.

## Process topology

For each daemon-backed task:

1. `clawd` claims the task and spawns `claw-agentd` as the task uid with the
   package-created `cos-extension` primary gid.
2. `clawd` leases a package-created locked uid that no other active task may use, creates a
   private runtime directory and route-filtered broker socket, then spawns
   `claw-extension-host` with that uid and the dedicated gid.
3. `clawd` registers the exact host pid/start-time as a child of the task
   session and signs the host identity, worker identity, socket paths, nonce,
   protocol version, and lease deadline into the worker grant.
4. `claw-agentd` verifies that binding and uses the host control socket for
   one-shot Apps, stateful App sessions, and configured MCP servers.
5. Hosted children use `COS_EXTENSION_BROKER_SOCKET`; they never receive or
   open `/run/cos/clawd.sock`.

The worker-control socket accepts only the exact worker pid/start-time and an
unused request id bound to the task/session/nonce. The broker proxy verifies
Linux `SCM_CREDENTIALS` on every frame:

- the exact host may call only App/MCP registration, bind, transient-scope,
  teardown, and permission-status routes;
- a descendant may call only normal `Session`/`PeerSession` provider routes;
- its request must name its nearest live App/MCP session, whose parent is the
  registered host session.

Another uid, task, session, recycled pid, or child that names its host/sibling
session is refused before normal broker dispatch.
Accepted requests still pass typed decoding, admission ceilings, capability
authority, provider-side checks, and audit.

## Isolation controls

The host launch applies:

- `setgroups(0, NULL)`, then irreversible uid drop to an exclusively leased
  package-created identity and gid drop to `cos-extension`;
- a pinned snapshot of the real primary broker socket inode, ownership, mode,
  and canonical ancestors, plus an actual denied post-drop `connect(2)` before
  either worker or host may exec;
- kernel re-verification of real/effective/saved ids and groups;
- `PR_SET_NO_NEW_PRIVS`, `PR_SET_PDEATHSIG`, `PR_SET_DUMPABLE=0`;
- `setsid()` and a dedicated process group;
- `umask(0077)`;
- empty environment rebuilt from an explicit non-secret allowlist;
- close-on-exec for every inherited descriptor;
- finite address-space, process, file-size, and descriptor limits;
- a mandatory private mount namespace with private propagation and fresh
  tmpfs mounts for `/tmp`, `/var/tmp`, `/dev/shm`, and `/run/lock`; the
  remaining mount tree is read-only except for the pinned task directory;
- best-effort IPC/UTS namespaces;
- mandatory cgroup-v2 CPU, memory, OOM-group, and pid limits.
- inherited seccomp `EPERM` denial for `ptrace`, process-vm read/write, `kcmp`,
  and `pidfd_getfd`;
- `PR_SET_DUMPABLE=0` at the first worker/host application entry point.

Every App and stdio MCP child then enters its own bubblewrap PID, network, and
mount namespaces. Loopback starts down, ambient TCP/UDP and abstract Unix
sockets are unreachable, `/proc` contains only that child namespace, and the
root is an empty tmpfs. The runtime view contains exact pinned executables and
ELF dependencies, filtered immutable language-runtime snapshots, generated
minimal account files, a read-only snapshot of explicitly authorized
extension code, exact broker/session endpoints, and private writable
state/tmp paths. Broad live `/usr`, `/etc`, and `/usr/local`, host homes,
arbitrary `/var`, `/mnt`, `/media`, extra mounts, and numeric-UID orphan files are
absent. Native stdio MCP networking is unsupported until a scoped authenticated
broker proxy exists; Apps use SDK/broker capabilities instead.
The trusted app runner waits for the root-maintained session bind and reapplies
non-dumpability immediately before final exec; isolation does not rely on
dumpability surviving exec.

Runtime path setup is descriptor-relative. The runtime, per-owner, and task
directories stay root-owned and non-writable. `openat2` rejects symlinks,
magic links, and path escape; creation, metadata changes, stale-tree removal,
and teardown use pinned directory FDs with `mkdirat`, `fchown`/`fchmod`,
`fstatat(AT_SYMLINK_NOFOLLOW)`, and `unlinkat`. The root-owned private broker
socket is bound before any directory is transferred, must remain a
single-link Unix socket with stable device/inode identity, and must map to the
listener inode in `/proc/net/unix`. Only the final control subdirectory is
then made writable by the host identity. A symlink or hardlink is unlinked,
never followed by privileged ownership or mode changes.

The package creates locked system accounts `cos-ext-00..63` with fixed UIDs
`61000..61063`, `/nonexistent` homes, `/usr/sbin/nologin` shells, and the
dedicated `cos-extension` primary group. Fresh installs use GID `60999`; a
provably unused legacy package GID may be retained during upgrade rather than
rewritten. The reserved identities remain below systemd DynamicUser
(`61184..65519`) and above the supported default login range.
Preinstall checks exact NSS and shadow records, systemd-homed, reverse UID/GID
lookups, and overlapping subordinate-ID ranges. Every runtime allocation
revalidates those records, the package reservation manifest, and `/proc`; any
collision or live process fails closed.
The uid remains reserved until cgroup cleanup, private filesystem unmount,
recursive task-state deletion, and routed-registry ACL revocation all succeed.
Before task state is created, `clawd` writes a durable root-owned quarantine
record. A failed cleanup retains the cross-process uid lock for the daemon
lifetime; after restart, recovery first kills stale cgroups and then proves
that processes, `/run/user/<uid>`, task state, and routed ACLs are gone before
removing the record. The proxy authenticates the actual extension uid but
projects only the already-bound task-owner principal through normal
capability/approval enforcement. Home, data, cache, and log locations are
task-local controlled directories. A custom MCP command is resolved before the
wrapper is built: system-wide `/opt` commands must be root-owned and
non-writable, while owner-private commands must live below the explicitly
authorized package root. Code is copied with no-follow inode/time rechecks;
the command, trusted script interpreter, and exact ELF dependency closure are
the only executable runtime inputs. Missing interpreters or libraries fail
before spawn.

Before spawning a host, `clawd` establishes a delegated cgroup-v2 subtree with
the CPU, memory, and pids controllers. The host writes its own pid through a
pre-opened `cgroup.procs` descriptor in `pre_exec`, before dropping privilege
or executing the host image, and the parent verifies the exact membership.
Creation also proves that every limit was applied and that `cgroup.kill` works.
If any step is unavailable or unwritable, the task fails before worker
assignment; dynamic code is never started without containment.

Teardown closes the private proxy and revokes child sessions first, then
requires `cgroup.kill`, recursive `populated 0`, an empty `cgroup.procs`, and
successful cgroup removal. It then unmounts every private tmpfs, recursively
removes the descriptor-pinned task tree without following symlinks or crossing
mounts, verifies the tree is absent, and revokes routed ACLs. Open file
descriptors do not retain directory entries; hardlinks and symlinks are
unlinked rather than followed; nested mountpoints make cleanup fail closed.
It does not infer descendants from post-exit `/proc` ancestry. A child remains
contained after `setsid`, double-forking, host-first exit, or clearing
`PDEATHSIG`; cleanup failure is terminal, logged, audited, and quarantined
rather than reported as a clean task completion.

The retry boundary is the authenticated PREPARE/COMMIT gate. PREPARE leaves the
worker blocked. Only after the current queue schema and exact lease/nonces are
durably committed may the broker send COMMIT. Legacy, unsupported, malformed,
or ambiguous Pending records are terminal indeterminate; queue directories are
created bottom-up with each new directory and parent fsynced before submissions
are accepted. Any COMMIT persistence/delivery ambiguity or subsequent
worker/host EOF, crash, handshake, or lease timeout is terminal; Claw OS does
not replay without operation-specific durable idempotency proof.

## Lifecycle

Control frames are versioned, size-bounded, one request per connection, and
limited to a fixed concurrency. Request ids have a bounded replay window.
Attach and call handshakes have deadlines. A timed-out or cancelled blocking
operation causes the host to exit so the broker can kill the complete process
tree rather than leave an unknown child alive.

The lifecycle is deterministic:

```text
attach -> ready -> call* -> detach
                 \-> cancel | timeout | crash
task completion -> shutdown -> proxy/session revocation
                 -> cgroup.kill -> verified empty -> detach
```

App/MCP identity, manifest/config digest, calls, outcomes, timeout/crash/cancel,
and host attach/detach are projected into the broker audit trail. App session
grant issuance records the exact delegated capability set. Relevant lifecycle
events are also appended to durable session mutations. The model sees only
fixed local `mcp_catalog` and `mcp_invoke` descriptors. Remote names and
argument-property names are disclosed inside wrapped untrusted data with
opaque owner/session/task/generation-bound handles; calls carry the canonical
sanitized descriptor-set digest and fail closed on drift or replay. Each handle
also binds the original internal MCP policy identity, transport, extension,
and exposure requirements. Catalogue output omits guardrail-hidden and
auto-denied entries, and invocation re-enters the shared registry
guardrail/approval path before remote execution.
Stdout/stderr-derived errors and tool results remain untrusted and wrapped.

## Configuration

| Variable | Meaning |
| --- | --- |
| `COS_EXTENSION_HOST_BIN` | Host executable; defaults beside `clawd`, then `/usr/local/bin/claw-extension-host` |
| `COS_EXTENSION_EXEC_GROUP` | Dedicated execution group; packaged default `cos-extension` |
| `CLAWD_EXTENSION_CGROUP_ROOT` | Optional pre-created empty, root-owned delegated cgroup-v2 root; normally omitted so `clawd.service` prepares its own delegated subtree |
| `CLAWD_EXTENSION_HOST_NAMESPACES` | Set to `off` to disable best-effort IPC/UTS namespaces |

The binary is shipped in `claw-os-agent` and configured by `clawd.service`.
There is no supported mode that falls back to executing dynamic code inside
`claw-agentd`.

## Validation

```bash
cd core
cargo test -p cos extension_host -- --test-threads=1
cargo test -p cos --test extension_host_boundary -- --test-threads=1
cargo test -p cos --test agentd_process_boundary -- --test-threads=1
cargo clippy -- -D warnings

cd ..
bash packaging/deb/tests/test-agentd-packaging.sh
```

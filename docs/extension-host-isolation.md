# Dynamic Extension Host Isolation

Claw OS runs dynamic App and MCP code in `claw-extension-host`, not in
privileged `clawd` and not in the `claw-agentd` model/tool worker.

## Process topology

For each daemon-backed task:

1. `clawd` claims the task and spawns `claw-agentd` as the task owner.
2. `clawd` creates a private runtime directory and route-filtered broker
   socket, then spawns `claw-extension-host` as the same owner.
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

A same-uid sibling, another task, another session, a recycled pid, or a child
that names its host/sibling session is refused before normal broker dispatch.
Accepted requests still pass typed decoding, admission ceilings, capability
authority, provider-side checks, and audit.

## Isolation controls

The host launch applies:

- `setgroups(0, NULL)`, then irreversible gid/uid drop;
- kernel re-verification of real/effective/saved ids and groups;
- `PR_SET_NO_NEW_PRIVS`, `PR_SET_PDEATHSIG`, `PR_SET_DUMPABLE=0`;
- `setsid()` and a dedicated process group;
- `umask(0077)`;
- empty environment rebuilt from an explicit non-secret allowlist;
- close-on-exec for every inherited descriptor;
- finite address-space, process, file-size, and descriptor limits;
- best-effort IPC/UTS namespaces;
- best-effort cgroup-v2 CPU, memory, and pid limits.

If cgroup creation is unavailable, teardown repeatedly walks `/proc`, kills
descendants before the host, and kills the host process group. The host is also
a child subreaper, so daemonized descendants remain attributable.

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
task completion -> shutdown -> descendant cleanup -> session/grant revocation
```

App/MCP identity, manifest/config digest, calls, outcomes, timeout/crash/cancel,
and host attach/detach are projected into the broker audit trail. App session
grant issuance records the exact delegated capability set. Relevant lifecycle
events are also appended to durable session mutations. Returned descriptors,
stdout/stderr-derived errors, and tool results are untrusted and are wrapped
before entering model context.

## Configuration

| Variable | Meaning |
| --- | --- |
| `COS_EXTENSION_HOST_BIN` | Host executable; defaults beside `clawd`, then `/usr/local/bin/claw-extension-host` |
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

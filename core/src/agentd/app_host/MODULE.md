# Controlled App Host

## Purpose

Let the existing unprivileged, leased Agent worker host ordinary App operations
and App-owned stateful sessions through a closed control channel. App code runs in the
shared hostile-worker sandbox. This is not a separate App product category,
an unsandboxed launcher, or access to the general broker socket.

## Key Files

| Path | Role |
| --- | --- |
| `protocol.rs` | Closed operation/session preparation, call start/end, registration, bind, relay, release, approval-status and live-capability messages |
| `broker.rs` | Root-owned original invocations, session/package/process tracking, standard admission and deferred cleanup |
| `broker/stateful.rs` | Retained session calls, call IDs and task-local aliases for freshly authorized grant rotations |
| `../worker.rs` | Process-local gateway, correlated replies and independent channel heartbeats |
| `../../operations/invocation.rs` | Original invocation and canonical preparation value definitions |
| `../../clawd/app_sessions/task_host.rs` | Private task-host registration using ordinary App authority and permission policy |
| `../../bridge.rs` | Manifest-selected App execution with a retained invocation scope |
| `../../agent/tools/cos_apps_session/` | Streamed shared sandbox, serialized call/clear lifecycle and receipt capture |
| `../../../test/unit/agentd/app_host/` | Protocol, authority integration and process-lifecycle regressions |

## Boundaries

- The signed task grant and current lease select the owner and parent session.
  No request can select an owner, inherit a serialized grant, or approve work.
- Begin retains the original App/operation/args/package digest at root and
  returns a generated task-owned identifier plus canonical argv. Registration
  references that identifier; it never defines its own comparison baseline.
  Runtime-selected arguments must be explicit in the original invocation;
  unresolved selectors are not silently filled with wider targets.
- The private registration exception requires a live, identity-bound,
  non-root `NoNewPrivs` worker. Public registration continues to reject
  `NoNewPrivs` callers.
- Normal controls reuse the broker's shared admission limits, route decoding,
  capability authority, mutation journal and audit. Relay remains limited to
  the session-scoped system-service routes the App grant permits.
- The broker owns provenance runtime writes. Before bind it rechecks the
  retained snapshot; after bind it reads back the exact package, App class,
  UID, PID, start time and cgroup. Liveness queries cannot substitute for this
  process-binding check.
- The non-App task itself does not acquire the extension runtime's writable
  lock for capability checks. Its sandbox runtime scratch belongs to the
  authenticated owner, never to the daemon's root data directory.
- Cancellation closes admission immediately, but does not drop an admitted
  privileged mutation. Cleanup waits for it, revokes owned grants, terminates
  only the captured process identity, then removes matching records.
- Control messages are bounded to 1 MiB, with eight in-flight controls and
  sixteen active invocations/sessions per task. The control budget is 4096
  requests per worker.

Stateful sessions bind the signed `session.entry` and run in the same
WorkerSandbox as ordinary operations, with streamed stdio and server limits.
They keep only their package, runtime and private App data mounted. A tool's
`needs` do not become lifetime host-file mounts or network access; mediated
providers use the current call's capabilities.

Each call retains its original tool/arguments at root, receives canonical
arguments and a root-generated call ID, and freshly authorizes its exact
requirements. The private task-host path rotates launch/App/relay grants,
rather than widening a child through generic attenuation. Actual handles stay
at root; stable aliases are valid only inside their owning task and session.
The App is at base invoke authority between calls. Active-call grants have a
short deadline and cannot extend the original session lifetime.

Grant, RPC and clear share a per-session lock. An overlapping start or a
delayed end for an older call is refused without clearing a newer call.
Timeout, transport uncertainty or failed clearing retires the session, never
replays the operation. Root live-capability queries also check the actual
grant, so broker endpoint threads do not depend on inherited task-local paths
or trust stale serialized capabilities.

Worker protocol v7 adds these private controls. General external MCP servers,
GUI launches, native-host exemptions and arbitrary broker calls are not
accepted through this surface. App-owned sessions remain ordinary Apps, not a
new product category.

## Validation

Run focused unprivileged tests from the repository root on Linux or WSL:

```bash
cargo test -p cos --lib -- agentd::app_host agentd::worker \
  clawd::app_sessions::task_host --test-threads=1
```

Registration deliberately uses the canonical root-owned
`/run/cos/caps/<uid>` path. The privileged controller regressions are opt-in
and must not use the machine's live `/run`. Build the unit binary, then run it
with a private mount namespace and a fresh `/run` tmpfs:

```bash
binary=$(cargo test -p cos --lib --no-run --message-format=json | python3 -c \
  'import json,sys; rows=[json.loads(line) for line in sys.stdin]; print([r["executable"] for r in rows if r.get("reason")=="compiler-artifact" and r.get("executable") and r.get("profile",{}).get("test")][-1])')
sudo unshare --mount --propagation private --fork bash -c '
  mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run &&
  touch /run/.cos-app-host-test &&
  exec "$1" agentd::app_host::broker --include-ignored --test-threads=1
' app-host-tests "$binary"
```

The tests require root and verify the namespace/marker before registering
anything. Do not weaken canonical-path or trust-file checks to make a fixture
run on DrvFS.

The real worker/App/Activity test also needs a freshly built `cos` and an
existing non-root owner. From that owner's shell, reuse `binary` above:

```bash
cargo build -p cos --bin cos
sudo env COS_APP_HOST_COS_BIN="$PWD/target/debug/cos" \
  COS_APP_HOST_OWNER_UID="$(id -u)" \
  unshare --mount --propagation private --fork bash -c '
    mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs /run &&
    touch /run/.cos-app-host-test &&
    exec "$1" agentd::supervisor::tests::app_host_process::controlled_host_runs_one_real_app_and_records_one_receipt \
      --ignored --exact --test-threads=1
  ' app-host-process "$binary"
```

Only the orchestration driver is a test fixture: the worker privilege drop,
App sandbox, policy checks, root-owned file replacement, receipt persistence
and recording-only retry execute through the real implementation. Test trust
roots live inside the private namespace; no live user trust store is changed.

Run the same process command with
`agentd::supervisor::tests::app_host_process::controlled_host_keeps_one_stateful_app_with_per_call_authority_and_receipts`
to cover two real file replacements in one persistent App, changing exact
call scopes, idle denial, isolated filesystem/network, shared receipts and
recording-only retry.

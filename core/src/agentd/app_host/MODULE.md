# Controlled App Host

## Purpose

Let the existing unprivileged, leased Agent worker host ordinary one-shot App
operations through a closed control channel. App code still runs in the
shared hostile-worker sandbox. This is not a separate App product category,
an unsandboxed launcher, or access to the general broker socket.

## Key Files

| Path | Role |
| --- | --- |
| `protocol.rs` | Closed Begin/End, registration, bind, relay, release, approval-status and liveness messages |
| `broker.rs` | Root-owned original invocations, session/package/process tracking, standard admission and deferred cleanup |
| `../worker.rs` | Process-local gateway, correlated replies and independent channel heartbeats |
| `../../operations/invocation.rs` | Original invocation and canonical preparation value definitions |
| `../../clawd/app_sessions/task_host.rs` | Private task-host registration using ordinary App authority and permission policy |
| `../../bridge.rs` | Manifest-selected App execution with a retained invocation scope |
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

This initial surface supports one-shot operations. GUI, native-host
exemptions, arbitrary broker calls and stateful MCP session controls are not
accepted.

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

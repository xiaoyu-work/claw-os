# Desktop Agent UI Module

## Purpose

`desktop/agent/ui/` is the native libcosmic Agent client. It presents the
versioned desktop Agent protocol without importing clawd or core models.

## State ownership

| Module | Ownership |
| --- | --- |
| `src/main.rs` | Application assembly, top-level message routing, subscriptions, and startup |
| `src/session.rs` | Local sessions, history reconciliation, retry branches, and transcript models |
| `src/stream_state.rs` | Generation-aware stream reduction, terminal states, cancellation, and stale-event rejection |
| `src/bridge_state.rs` | Bridge connection, model availability, failure, and reconnect state |
| `src/effects.rs` | Async bridge, history, stream, and cancellation effects that emit typed UI messages |
| `src/voice.rs` | Recording/processing lifecycle, abort generation, and stale completion rejection |
| `src/overlay.rs` | Deferred context submission, file-picker focus, and layer-surface lifecycle; activation type is owned by `cos-runtime` |
| `src/views.rs` | Read-only widget composition that emits `Message` values |
| `src/styles.rs` | Presentation styles |
| `src/bridge.rs`, `src/sse.rs`, `src/recorder.rs` | Protocol transport, SSE decoding, and audio capture/upload adapters |

State modules do not call one another through a service locator or global
mutable state. `main.rs` composes their typed transitions and dispatches
effects. The stream reducer is the only owner of active, terminal, cancelled,
and stale stream-event handling.

## Dependencies

The UI consumes DTOs from `../protocol/` through `src/bridge.rs`. Views may
read composed application state and emit messages, but transport orchestration
belongs to `src/effects.rs` and lifecycle owners.

Initial CLI arguments and subsequent single-instance D-Bus activation use
`cos_runtime::ask_claw::{UiArguments, Activation}`. Keep executable names,
overlay flags, and context serialization out of the UI and host apps. Shared
launches carry a bounded serialized activation over stdin only when
`--context-stdin` is explicit; the new process reads and validates it before
starting a dedicated transient overlay. Context-bearing activation never uses
the unauthenticated well-known D-Bus name; only context-free overlays retain
single-instance forwarding. Payload-bearing `--context` and `--query` are
rejected; `--context-stdin` plus the inherited readiness descriptor is the only
private activation path.

## Tests

Private-access unit tests mirror production modules under `test/unit/`.

```bash
cargo test --manifest-path desktop/agent/Cargo.toml -p cos-agent-ui
cargo clippy --manifest-path desktop/agent/Cargo.toml -p cos-agent-ui -- -D warnings
```

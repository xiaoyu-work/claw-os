# Desktop Agent UI Module

## Purpose

`desktop/agent/ui/` is the native libcosmic Agent client. It presents the
versioned desktop Agent protocol without importing clawd or core models.

## State ownership

| Module | Ownership |
| --- | --- |
| `src/main.rs` | Application assembly, top-level message routing, subscriptions, and startup |
| `src/activities.rs` | Activity list/detail widgets, unsaved metadata/work/object forms, and generation-aware reduction of fetched responses; no lifecycle authority or persistence |
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

The standalone sidebar exposes Activities without changing the private
chat/voice overlay. Activity requests go through the same owner-scoped broker
service used by `cos activity`, via the bridge's versioned presentation DTOs.
Activity work uses durable `activity.run`, never the cancel-on-disconnect chat
stream. Job results cannot change goal state locally; explicit completion
requires the user's confirmation note. Pending approvals link to associated
sessions and remain decisions for the existing Approval Gate. Resource
references are displayed as inert text, and boundaries grant no capabilities.

The reducer keeps at most one request for the current generation. Navigation
invalidates stale responses without cancelling backend work. Visible views
poll every five seconds; edits, in-flight requests and visible request errors
suspend automatic refresh. Async calls remain in `effects.rs` and `bridge.rs`.

App object attachment sends typed components to the broker; this UI never
parses or formats their URIs. Declaration lookup is explicit and refreshed
after attachment or changes to inspected resources, not on every job poll.
The read-only results distinguish authenticated declarations from unavailable
or invalid references. A declaration proves neither object existence nor
readability. Normal operation details can be copied as labeled fields with
JSON argv, but never executed or resolved by the UI. Ordinary resources and
terminal Activity edit restrictions remain unchanged.

Effect previews use a declared object's existing invocation, not a new command
form. The shared broker owns manifest interpretation and argument binding.
Only returned metadata is displayed: App-declared effects, non-guaranteed
recovery, requested (not canonical) targets, unresolved arguments and notes.
Absent declarations mean unknown effects. Authorization, execution and effect
confirmation must remain explicitly false.

The preview is fetched only on a button press. Its Activity, object reference,
invocation snapshot and generation must still match when the reply arrives.
Metadata/object drafts and navigation invalidate it; no result starts a job
or becomes an approval. Preview response DTOs contain no raw non-resource argv.

## Dependencies

The UI consumes DTOs from `../protocol/` through `src/bridge.rs`. Views may
read composed application state and emit messages, but transport orchestration
belongs to `src/effects.rs` and lifecycle owners.

Initial CLI arguments and subsequent single-instance D-Bus activation use
`cos_runtime::ask_claw::{UiArguments, Activation}`. Keep executable names,
overlay flags, and context serialization out of the UI and host apps. Shared
launches carry one bounded length-prefixed activation over an inherited Unix
socket only when `--context-socket --activation-fd` is explicit; the new
process becomes non-dumpable, signals readiness, then reads and validates it
before starting a dedicated transient overlay. Context-bearing activation never uses
the unauthenticated well-known D-Bus name; only context-free overlays retain
single-instance forwarding. Payload-bearing `--context` and `--query` are
rejected; the inherited socket descriptor is the only
private activation path.

The install target and runtime launch target are both fixed at
`/usr/local/bin/cos-agent-ui`. The same recipe installs
`/usr/local/bin/cos-ask-claw-launcher` for public SDK bindings.

## Tests

Private-access unit tests mirror production modules under `test/unit/`.

```bash
cargo test --manifest-path desktop/agent/Cargo.toml -p cos-agent-ui activit -- --test-threads=1
cargo test --manifest-path desktop/agent/Cargo.toml -p cos-agent-ui
cargo clippy --manifest-path desktop/agent/Cargo.toml -p cos-agent-ui -- -D warnings
```

The matching `../protocol/` and `../bridge/` Activity tests cover additive v1
DTO defaults, owner-input rejection, schema translation, pending-approval
scoping, authenticated routes and explicit broker errors. Object regressions
also cover opaque components, retained diagnostics, no local attachment or
completion inference, stale lookup/attachment responses, terminal restrictions,
and inert operation details. Preview coverage includes metadata-only flags,
requested-target preservation, non-resource argument redaction, missing effect
declarations, selection/draft guards and unchanged Activity/job state.

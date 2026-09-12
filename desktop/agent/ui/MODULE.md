# Desktop Agent UI Module

## Purpose

`desktop/agent/ui/` is the native libcosmic Agent client. It presents the
versioned desktop Agent protocol without importing clawd or core models.

## State ownership

| Module | Ownership |
| --- | --- |
| `src/main.rs` | Application assembly, top-level message routing, subscriptions, and startup |
| `src/activities.rs` | Activity list/detail widgets, unsaved metadata/work/object forms, and generation-aware reduction of fetched responses; no lifecycle authority or persistence |
| `src/activities/object_state.rs` | Fixed Object state section, bounded caller-report drafts, resource selection, receipt links and immutable history presentation inside the Activity reducer |
| `src/activities/execution_limits.rs` | Fixed execution-constraints card, explicit refresh, lifetime-counter/status presentation, and revision-bound configuration/toggle handling; no authority or execution |
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

Activity receipts have an explicit read-only refresh and use the same shared
broker ledger as headless clients. Closed source/outcome/result enums prevent
caller reports from being relabeled as OS-confirmed or applied effects. The UI
shows recording time, reported bytes/digests, bounded result/error text,
truncation and optional matched declarations without parsing result markup.
Declaration snapshots were authenticated at recording time, not against
current App validity. Even with a snapshot, execution remains caller-reported;
historical metadata and recovery hints are not evidence or guarantees.
Reading receipts remains available for terminal Activities and
never changes jobs, goal state, object previews or permissions. There is no
receipt authoring/execution endpoint or local persistence.

The fixed Object state section reads and records annotations through
`activity.object_state.list/record`, never through a desktop store. Reads are
explicit and bounded to 100 entries, with an optional exact-reference filter.
History, including superseded/retracted entries and detached subjects, remains
visible for every Activity lifecycle state. All classifications are caller
reports; within-window validity is not truth or verified freshness.

Authoring selects existing App resource strings without constructing URIs.
Statements, inferences, receipt links and planning relations use closed protocol
variants. Receipt links reuse the existing inert bounded report renderer and
remain `caller_reported`, not evidence that a result concerns the subject.
Corrections/retractions append a fresh UUID with a fixed subject and predecessor;
the broker owns membership, same-owner/Activity/App receipt binding and
single-successor conflicts. Retracted entries cannot be corrected.

An unchanged retry retains the submission UUID; editing after an attempted
submission starts a new UUID. Pending writes freeze the form and never replay
on reconnect. Selection generations, filter snapshots and acknowledged draft
identity reject stale or mismatched replies. Success refetches shared history
without locally changing Activity, task, receipt or approval state. Acknowledgements
allow the broker's outer-text trimming, UUID canonicalization and UTC timestamp
normalization, but reject changed identity, references, content, predecessor or
instants. The protocol uses UUID/Chrono parsers for that comparison without
rewriting the retry payload. RFC3339 inputs remain text; admission, canonical
URI semantics and lifecycle authority remain broker-owned.

Execution limits are a separate fixed card and separate typed get/set/enabled
routes, not a new permission system. Reads are explicit, and missing/invalid
data never becomes an unconfigured-policy success. The card shows bounded
attempt/turn constraints, lifetime usage, remaining attempts, local-clock
expiry status and revision, while the broker alone admits and stops attempts.
First configuration is enabled at revision 1; edits preserve enabled state and
lifetime usage. Disabling prevents delegated work instead of restoring
unlimited execution. There is no reset/delete control.

The configuration form captures the fetched revision; neither refresh, errors,
nor edits rebase it. Set/toggle acknowledgements require exact revision
progression, matching Activity/owner, unchanged lifetime identity and
nondecreasing usage, while permitting normalized UTC expiry. The authenticated
bridge validates owner UID using its existing process-identity inspection;
the UI additionally compares existing snapshots. Initial creation accepts no
owner/counter/result selectors. Successful replies trigger a fresh get rather
than locally changing a counter, policy state or goal. Generation checks and
form exclusion preserve the existing Activity/receipt/object-state flows.

Only active/paused Activities can be configured or enabled. Disabling and
inspection remain available for terminal Activities. User-facing caveats
explain pre-worker durable charging, startup/crash consumption, fresh retry/
recovery attempts, actual model-turn enforcement, normal cancellation/lease
cleanup on expiry/disable/revision change and no promise to undo already-admitted
privileged mutations. These constraints never grant capabilities or approvals,
execute an App/model or transition a goal.

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
cargo test --manifest-path desktop/agent/Cargo.toml -p cos-agent-protocol -p cos-agent-bridge -p cos-agent-ui execution_limits -- --test-threads=1
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
Receipt coverage adds strict source/claim decoding, declaration absence and
diagnostics, inert output, stale selection, terminal reads and GET-only routing.
Object-state coverage under `test/unit/activities/object_state.rs` adds every
content/relation/validity variant, immutable correction/retraction history,
UTF-8 bounds, fixed resource selection, retry identities, detached/terminal
reads and stale read/write rejection. Protocol, bridge translation, route and
UI transport tests cover the matching list/record contract and owner/source
input rejection.
Execution-limit coverage under `test/unit/activities/execution_limits.rs` adds
unloaded/unconfigured/failed reads, enabled/expired/exhausted status, initial
null CAS, normalized expiry, lifetime usage and disabled-state preservation,
stale conflicts without rebasing, terminal-state rules, owner/revision
acknowledgements and stale selection. Matching protocol/translation/route/
transport tests cover required-nullable fields, selector rejection, bounded
inputs, authenticated owner scope and the absence of reset/delete endpoints.

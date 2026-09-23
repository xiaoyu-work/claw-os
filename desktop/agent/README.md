# ClawOS Agent App (`com.clawos.Agent`)

The user-facing face of the ClawOS system Agent. The brain (LLM
runtime, providers, tools, caps, memory) lives in `core/src/agent/`
and is orchestrated by the user-session `clawd` daemon. This directory holds the
**desktop UI surface** plus the small local HTTP bridge that
brokers a streaming JSON+SSE protocol between the UI and `clawd`.

## Layout

```
desktop/agent/
├── Cargo.toml              # workspace: bridge + ui
├── protocol/               # shared versioned HTTP/SSE presentation contract
├── docs/
│   └── design-system.md    # shared dark-surface / brand-blue accent system
├── bridge/                 # cos-agent-bridge — HTTP+SSE daemon
│   └── src/
│       ├── main.rs         # 127.0.0.1 Axum server (/api only)
│       ├── state.rs        # port discovery + shared clawd client
│       ├── notifications.rs # clawd delivery lease → session D-Bus popup
│       └── routes/
│           ├── chat.rs     # POST /api/chat   (SSE stream)
│           ├── activities.rs # shared Activity service + durable work admission
│           ├── sessions.rs # canonical list/get/update/fork/history + legacy read-only projection
│           ├── models.rs   # GET /api/models
│           └── voice.rs    # POST /api/voice/upload → configured STT provider
└── ui/                     # cos-agent-ui — native libcosmic chat
    ├── src/
    │   ├── main.rs         #   Application assembly, routing, subscription
    │   ├── activities.rs   #   Fetched Activity views, forms and request generations
    │   ├── session.rs      #   Session/history domain and reconciliation
    │   ├── stream_state.rs #   Generation-aware stream/cancel reducer
    │   ├── bridge_state.rs #   Connection/model lifecycle state
    │   ├── effects.rs      #   Async bridge and stream effects
    │   ├── voice.rs        #   Recording/transcription lifecycle state
    │   ├── overlay.rs      #   Activation and layer-window state
    │   ├── views.rs        #   Read-only widget composition
    │   ├── styles.rs       #   Presentation styles
    │   ├── bridge.rs       #   port discovery + wire types
    │   ├── sse.rs          #   reqwest-based SSE consumer
    │   └── recorder.rs     #   bounded capture, resampling, levels + WAV upload
    └── assets/             #   brand PNGs baked into binary
```

## Runtime topology

```
┌─ standalone window ─┐   ┌─ Super+A layer overlay ─┐
│  cos-agent-ui       │   │  cos-agent-ui      │
│  (native libcosmic) │   │  --overlay         │
└─────────┬───────────┘   └─────────┬──────────┘
          │                         │
          └────────────┬────────────┘
                       ▼
           ┌──────────────────────────┐
           │  cos-agent-bridge        │
           │  127.0.0.1:$PORT         │
           │  /api/* + notification   │
           │  delivery subscriber     │
           └────────────┬─────────────┘
                        │
                        ▼  Unix socket
           ┌──────────────────────────┐
           │  clawd                   │
           │  task + notification RPC │
           └──────────────────────────┘
```

The bridge also claims pending desktop deliveries from the owner-scoped
Notification Service, posts them to the graphical session's
`org.freedesktop.Notifications` D-Bus service, and reports delivery success or
retryable failure to `clawd`. This reverse bridge keeps the system daemon out
of the user's session bus.

The bridge and approval applet share `crates/clawd-client` for canonical
`CLAWD_SOCKET` discovery (`COS_CLAWD_SOCKET` remains a compatibility alias),
v2 broker envelopes and request correlation, `CBK1` length-prefixed framing,
deadlines, bounds, and typed transport/protocol errors. This broker wire
version is independent of desktop HTTP/SSE presentation protocol v3. The
paired UI/bridge minimum is v3 because an older bridge can either discard image
attachments or cancel a durable Job when its viewer disconnects. Activity detail
also carries the shared `activity.attention` projection; the bridge validates
its Activity identity and derives the legacy pending-approval presentation only
from exact pending decisions in that projection. Grouped notification
occurrence counts are presentation data from the same projection, not a local
desktop batch or delivery queue.

The shared client inventory also retains mainline's protected
[`system.review.*` contract](../../crates/clawd-client/MODULE.md), including
root-peer checks, and the notification acknowledge/dismiss routes. Activity
constraints do not replace the existing review presenter or create consent.
The bridge keeps the mainline notification consumer: it binds presentations
to the installed executable and unique D-Bus sender, reflects durable
acknowledgement/dismissal, and retires timed-out/disconnected handles without
acknowledging their records. Activities add no second delivery loop.

The UI and bridge both compile against `protocol/` (`cos-agent-protocol`).
That crate exclusively owns the desktop presentation contract: endpoint DTOs,
named SSE payloads, stable error envelopes, discovery metadata, and protocol
version constants. Its only other dependencies beyond Serde/`serde_json` are
UUID and clock-disabled Chrono parsing, used to compare normalized Activity
acknowledgements without importing broker/domain code. The bridge
remains the anti-corruption layer: `bridge/src/translation.rs` decodes generic
clawd results, removes worker/task storage details and raw memory content, and
emits only protocol types. The UI does not deserialize clawd or core models.

Conversation inventory comes from the same owner-scoped
`agent.conversation.*` service as Agent Web. Native search is presentation-only;
rename, archive/restore and fork cross the bridge as typed requests and remain
broker mutations under the idle-session lock. Memory-only legacy sessions are
merged into the list as visibly read-only entries and continue through
`memory.history`; Desktop never opens the Agent database.

Within the UI, lifecycle state is split by invariant owner. `main.rs` routes
typed messages among session, stream, bridge, voice, and overlay state;
`effects.rs` performs transport work; and `views.rs` only reads state and emits
messages. The Activities reducer and its read-only widgets live together in
`activities.rs`. Stream generations and cancellation remain centralized in
`stream_state.rs`, so stale events cannot mutate a newer request.

Tool input is the protocol's only intentionally open JSON field. Tool schemas
are registered dynamically by the runtime, so their payload cannot be closed
over in this dependency-light crate; the boundary is documented by
`ToolInput`.

The bridge no longer serves a static SPA — the previous React
frontend was retired in favour of `cos-agent-ui`. Every UI surface
talks only to the `/api/*` endpoints.

The overlay is a single-instance Wayland layer-shell surface:

- `Super+A` opens the compact multiline summon composer.
- `Super+Shift+A` opens the live voice orb and begins recording.
- Re-invoking either shortcut reuses the existing overlay process.
- App-provided private context opens a separate transient overlay so its
  activation is never forwarded through the well-known D-Bus name.
- Escape stops/cancels active work before closing the surface.

Chat streams expose task identity, live text, tool lifecycle, warnings,
usage, and final metadata. Stop explicitly cancels the clawd task. Dropping the
client stream only detaches that viewer; reopening its canonical conversation
uses verified task bindings to reconstruct the prompt and replay
`task.stream`. While a stream is active, follow-ups are durable
`after_task_id` Jobs whose session is derived from the owner-checked
predecessor rather than from UI input.

Native image attachment uses desktop presentation protocol v3. The file chooser
reads only an explicitly selected PNG/JPEG/GIF/WebP image, enforces the shared
four-image/256-KiB bounds, and sends inline data through the bridge to the
canonical `task.submit` attachment contract. It no longer inserts a local path
marker into the prompt. The bridge owns no attachment store and derives no file
authority from the selected path.

Voice uploads are staged as private runtime files and transcribed via
the configured `cos model transcribe` provider. App/window context is handed to the UI through an inherited AF_UNIX socketpair
rather than argv, environment, pipes, or a pathname. Public SDKs first connect
to the packaged helper's abstract Unix listener; the helper authenticates its
captured direct parent with `SO_PEERCRED`. The host, helper, and UI fail closed
without strong Yama ptrace isolation and become non-dumpable. A readiness
handshake ensures the parent writes nothing until the UI is hardened. The new
process validates the typed activation and runs a dedicated transient overlay,
avoiding plaintext context on the unauthenticated single-instance D-Bus path.
It then sends context through an untrusted-data system boundary without
storing it as the visible user prompt.

The UI install and desktop package recipes target
`/usr/local/bin/cos-agent-ui` and install the
cross-language SDK entry point at `/usr/local/bin/cos-ask-claw-launcher`;
private context launches accept no executable override or `PATH` lookup.

## Activities

The standalone Agent window has an **Activities** entry above Sessions. It
lists the same owner-scoped goals as `cos activity` and the Agent Web client;
there is no desktop Activity database or separate lifecycle implementation.
The compact private overlay remains chat/voice-only.

Create or edit a title, goal, completion criteria, boundaries and resource
references; inspect associated jobs, their bounded result previews, sessions,
and pending approvals. References are inert text. Boundaries are planning data,
not capability grants. Permission decisions remain in the existing desktop
Approval Gate; session links open the existing conversation view.

Pause/resume, cancel/reopen and completion are explicit broker requests.
Completion requires a nonempty user confirmation; neither a successful job
nor an answer completes a goal. Pausing or cancelling a goal does not undo or
cancel its in-flight jobs; their separate Stop/Retry controls use the task API.

**Start / continue work** calls `activity.run`, not the chat SSE endpoint.
Once admitted, the job survives closing the view or window. A session can be
selected for continuation, or work can start in a new session. The UI only
holds fetched DTOs, unsaved forms and request state. It refreshes while visible
(every five seconds), suspends polling during edits or pending requests, and
ignores stale-generation responses. Failures are visible and do not synthesize
success; approval-queue failures are reported separately from the goal detail.
Reconnect re-discovers the authenticated bridge without replaying a mutation.
See the shared [Activity contract](../../docs/activities.md).

### App object references

**Attach App object** accepts a label, App ID, object type, opaque object ID,
and optional revision while the Activity is active or paused. The desktop
sends those components to `activity.object.attach`; only the shared broker
verifies the signed declaration and formats/upserts the canonical reference
in the existing resources list. Ordinary file/URL resources and their editor
are unchanged. There is no new desktop persistence or Activity metadata shape.

**Describe App objects** calls `activity.objects` without executing an App or
reading object data. `Declared` is displayed as **Verified declaration only**:
it authenticates the manifest's object-type declaration, not object existence,
freshness, readability or permission to access it. Unavailable and malformed
references retain their diagnostics. Completed/cancelled Activities can still
be described, but must be reopened before attachment.
Original resource text and the catalogue's canonical reference are displayed
separately when they differ; the desktop does not interpret URI equivalence.

Descriptions show the normal App operation and its arguments as a JSON argv
array. **Copy operation details** copies labeled fields and JSON, not a shell
command; there is no execute/open action or automatic object resolution. Lookups are
explicit, with metadata and descriptions refetched after attachment or changes
to already-inspected resources. Generation checks reject stale responses, and
failed attachments preserve the form without inventing a local resource.

### App-declared effect previews

**Preview App-declared effects**, beside a declared object's JSON argv, sends
that existing invocation to the shared `activity.operation.preview` service.
There is no generic execution form. The response is metadata from the signed
App manifest: App/version/package digest, declared effects, requested targets,
unresolved arguments and notes. Missing effect declarations explicitly mean
**unknown**, not read-only or safe to run.

The preview does not execute App code, read object data or credentials, check
execution permissions, confirm effects, or approve work. Targets are inert
requested values, including paths that are not final canonical resources.
Recovery categories are App-declared guidance, never a guarantee. The bridge
rejects replies that claim authorization, execution or confirmed effects and
excludes raw non-resource argv and unrelated broker fields from preview responses.

Previews are fetched only on request, including for terminal Activities.
Selection generations and an invocation snapshot reject stale replies;
navigation, object refresh and metadata/object editing discard displayed
previews. No preview enters the separate work-submission or approval path.

### Caller-reported execution receipts

The Activity detail's **Refresh receipts** button reads up to 100 immutable
reports from the same owner-scoped `activity.receipts` service as the terminal.
This is a GET-only presentation: it cannot author a receipt or execute an App.
Reports remain readable on paused, completed and cancelled Activities.

Every record is labeled **CALLER-REPORTED**. Outcomes are only **Returned**,
**Reported error** or **Indeterminate**; none means applied/verified effects
or a completed goal. Recording time is when the broker received the report,
not an execution timestamp. Reported format, original byte count and SHA-256
are displayed with bounded/redacted previews and truncation indicators.
The digest is not a cryptographic OS execution attestation. Result previews
and errors are inert plain text, including JSON, markup and command-like text.

An optional declaration is a historical snapshot authenticated **at recording
time**, never proof of current App validity, reported execution or outcome.
Later package changes or revocation do not turn an old snapshot into current
validation. The report remains **CALLER-REPORTED**; effect/recovery guidance is
historical, App-declared and non-guaranteed. If no declaration could be matched
when recording, the stored report and declaration diagnostic remain visible.
OS-confirmed mutation evidence belongs to the separate journal, not these
reports. Receipt refresh is explicit; stale selections and failed requests
cannot synthesize reports or change jobs, permissions, previews or goal state.
The core owns the schema-2 database migration; Activity and receipt wire
schemas remain 1, and the desktop never opens or migrates that database.

### Caller-reported object state

The fixed **Object state** section reads the same owner-scoped
`activity.object_state.list` history as terminal and Web clients. Refresh is
explicit, includes superseded/retracted entries and remains available after
resource detachment or pause/completion/cancellation. An optional exact App
reference filter narrows the response; each read requests at most 100 entries
from the shared, 1000-entry-per-Activity ledger. The frozen API has no cursor,
so the desktop does not invent pagination or a local history store.

Add a user statement, caller-classified agent inference, existing receipt ID
or planning relation. Subject and relationship target selections use existing
App resource strings; the desktop never constructs a canonical URI. The broker
validates canonical membership and the linked receipt's owner, Activity and
App. **All classifications are caller reports, not verified facts.** An
App-linked receipt is still `caller_reported`, not proof that its result
concerns the linked object. It uses the existing inert bounded result/error
renderer, including indeterminate and truncation caveats, without copying or
fetching original App data.

Optional reported-window start/end fields accept RFC3339 text. Both must be
absent or present; the broker validates timestamp syntax and end-after-start.
Relations/retractions have no time window. Unknown, not-yet-applicable,
within-reported-window and expired statuses remain reports, never proof of
truth or freshness. Window status is the broker's projection at the last
object-state refresh, and recording time is shown separately.

**Correct with a new entry** and **Retract with a reason** append a new UUID
with the same subject and a predecessor; they never edit/delete history.
Only an unsuperseded, non-retracted entry with an attached subject can be
superseded. Concurrent stale corrections surface broker conflicts. Retrying an
unchanged submission retains its UUID; editing after submission uses a fresh
UUID. Navigation/filter generations reject stale replies. An acknowledgement
must preserve Activity/entry UUID identity, exact subject/target references,
content kind, relation and predecessor, allowing only trimmed outer text,
equivalent UUID spellings and RFC3339 timestamps denoting the same instants.
UUID/Chrono parsers perform those comparisons without constructing App URIs or
rewriting the retry payload. Shared history is then refetched.
Nothing here invokes an App/model, changes lifecycle or confirms goal completion.
See the shared [object-state contract](../../docs/object-state.md).

### Explicit execution limits

The fixed **Execution limits** card uses the same owner-scoped
`activity.execution_limits.get/set/enabled` service as terminal and Web clients.
These settings are **constraints only, not capabilities or approval grants**.
The card never starts an App, model or job, changes goal state, or provides a
new permission-policy surface.

Refresh explicitly before configuring or toggling limits. An unloaded/failed
read is not interpreted as an unconfigured policy; only an explicit
`execution_limits: null` preserves existing standalone behavior. The card
displays enabled/expired status, lifetime used/maximum/remaining attempts,
maximum actual model turns per attempt, expiry, revision and creation/update
timestamps. Counters come from the last refresh; local-clock expiry display
never substitutes for broker admission.

Configuration has exactly three fields: attempts (1-1000), turns per attempt
(1-100), and RFC3339 expiry. The backend checks future expiry at transaction
time. Initial configuration sends `expected_revision: null`, starts at revision
1 and is enabled. Edits retain the fetched CAS revision even across failures;
stale conflicts never trigger automatic overwrites or rebasing. Replies are
checked against Activity identity, bridge owner identity and exact revision
progression while allowing UTC timestamp normalization.

Updates preserve enabled state and lifetime usage; concurrent reservations may
advance the count, never reduce it. A lower ceiling than the used count is
allowed and displays zero remaining attempts. Raising the ceiling is explicit,
not a reset. Turning limits off **disables delegated work**, rather than
removing the policy or restoring unlimited execution. No delete/reset endpoint
is exposed. Configuration/enabling require an active or paused Activity;
inspection and disabling remain available in completed/cancelled states.

Attempts are durably charged **before** worker startup. Startup/crash failures
can consume an attempt, and retries/recoveries are fresh attempts rather than
automatic refunds. Expiry, disabling or revision changes stop affected running
attempts through normal cancellation/lease cleanup. Already-admitted privileged
mutations are not promised to be undone. The backend owns those effects and
persistence; the desktop retains only fetched DTOs and unsaved revision-bound
forms. Successful mutations refetch current constraints without changing
Activity, receipt or object-state data locally.

### Configured monetary budget

The fixed **Monetary budget** card uses the same owner-scoped
`activity.monetary_budget.get/set/enabled` service as terminal and Agent Web
clients. Its closed protocol preserves integer micro-USD values and revisions
as `u64`, and mutations use the exact fetched revision. The card presents
configured rates, total, output limit, spent, reserved and remaining amounts,
while clearly separating configured accounting from provider prices, invoices
or billing reconciliation. The bridge derives owner identity from the
authenticated desktop process and accepts no caller-selected owner.

Absent policy is distinct from an invalid or failed read. Creation/update and
enable require active or paused Activities; terminal Activities remain
inspectable and can be disabled. Edits preserve backend ledger amounts and
enabled state, toggles preserve configured rates, and successful replies
refetch broker state. The protocol, bridge and UI own no policy, ledger,
persistence, provider pricing, job admission or goal-state authority.

### Scheduling priority

The fixed **Scheduling priority** card uses the same owner-scoped
`activity.scheduling_policy.get/set` service as terminal and Agent Web clients.
The bridge derives the current owner, accepts no owner selector, and forwards
only the Activity path identity, closed foreground/standard/background value,
and exact `u64` revision CAS. The native UI retains only the fetched DTO and an
unsaved revision-bound form; it has no scheduling database or Job-level
priority input.

Foreground is preferred for pending admission, standard preserves legacy
compatibility, and background may be deferred. After 30 minutes, aged queued
work precedes non-aged work in FIFO order. The card describes this as
admission-only metadata: it grants no authority or capability, bypasses no
consent or budget check, proves no execution or completion, does not preempt or
cancel running/approval-waiting work, and promises neither latency nor provider
QoS. Stale writes remain explicit conflicts and are never automatically
rebased.

### Activity capability policies

The fixed **Capability policy** card uses the owner-scoped
`activity.capability_policy.get/set/enabled` service. These policies constrain
controlled execution; they never supply capabilities or approval grants.
`normal` retains ordinary authorization, `require_approval` additionally
requires exact one-shot approval, and `deny` blocks the entire verb and
approval escalation. Missing verbs and empty rules add no constraints, not
permissions. Disabling a configured policy blocks controlled capability use
instead of removing the policy or restoring unrestricted behavior.

The editor has fixed typed rule rows, verb text, mode buttons, and path/host/
name/self-reference/explicit-wildcard scope controls. It is not model-generated
UI and does not expose a JSON command form. The broker validates known catalogue
verbs and compatible scopes; the native client does not copy the catalogue or
resolve resources. Resource-addressing verbs require explicit same-kind scopes
rather than raw Wild. Path scopes must already be canonical absolute patterns.
A deny row has no scopes; Normal/Ask rows require 1-32 scopes. There are at most
64 distinct rules and the complete JSON draft is bounded to 16 KiB.

Refresh is explicit. Only an explicit `capability_policy: null` is treated as
unconfigured; malformed replies are errors. Creation sends null CAS and starts
enabled at revision 1. Edits retain the captured revision and preserve enabled
state. Stale conflicts never silently rebase or replay mutations. Replies must
match owner, Activity, revision progression and submitted rule meaning. Matching
allows only the storage contract's ASCII host lowercasing, scope deduplication
and deterministic ordering. Other values and verb spelling remain exact; no
scope-coverage matcher or competing path canonicalizer is used.

Creating/editing/enabling is available only for active or paused Activities.
Inspection and disabling remain available in terminal states. Policy creation,
editing and revocation stop running old-policy attempts; admitted effects are
not undone. Ask applies to exact primitive, App invocation or session-call
confirmation even when permission is held. Approval decisions remain in the
existing gate; no grant or approval-decision route exists on this card.
Denying `fs.delete` alone does not prevent deletion via writes/exec or other
allowed verbs, and this feature makes no universal sandbox or undo guarantee.

The independent `crates/clawd-client` inventory includes all three routes in
its enum, Serde names, `ALL` and `as_str`; core route additions alone are not
sufficient to wire a desktop consumer.

### Portable Activity continuity

The native UI presents the published `activity.continuity.export/import`
service through closed desktop-owned DTOs. The protocol crate duplicates the
version-1 wire shape rather than importing core models. It validates the exact
kind/version, canonical UUID and SHA-256 snapshot, full required shape, `u64`
revision, canonical App references, execution/scheduling rules, and bounded
document, nesting, container, string, text and reference counts. Duplicate
keys, unknown fields, owner selectors and authority/proof fields fail closed.

Export revalidates the broker response before offering copy. The UI has no
vetted file-dialog integration for this surface, so it uses bounded JSON
copy/paste and performs no filesystem access. Copy always serializes the
broker-returned validated document, never pasted input. Import requires a
separate explicit **local placement** confirmation and creates a new paused
Activity. The acknowledgement must identify the authenticated owner, a new
canonical Activity UUID, the exact lineage/revision and local placement, and
the unchanged portable intent/references.

Continuity carries title, goal, completion criteria, planning boundaries,
canonical App object references, optional execution limits and scheduling
priority. It does not carry or restore owners, capabilities, approvals,
credentials, monetary/ledger state, jobs, sessions, receipts, object-state
observations, notifications, audit history, execution results, authority or
proof. The UI owns no persistence or database access, starts no work, restores
no authority, and provides no live sync. Navigation generations reject late
exports/imports, conflicts and validation failures remain visible, and import
does not mutate the source Activity.

## Endpoint discovery

The bridge binds an ephemeral port when `COS_AGENT_BRIDGE_PORT` is
unset (the systemd default), generates a random bearer token, and
atomically writes both values to
`$XDG_RUNTIME_DIR/cos-agent-bridge/endpoint.json` with mode `0600`.
The native UI reads this file and attaches the token to every bridge request.

## Protocol compatibility

Protocol v1 uses the `x-clawos-agent-protocol-version` request and response
header. Discovery also publishes `min_protocol_version` and
`protocol_version`. Missing, malformed, or unsupported versions fail with HTTP
426 and a typed
`incompatible_protocol_version` or `protocol_version_required` error; the UI
selects the highest version in the intersection of its compiled range and the
bridge discovery range. The bridge validates that selected version and echoes
it on every response; the UI rejects a missing or different echo.

The current binaries support exactly v3 (`min=3`, `current=3`), while the
intersection policy permits a future `min=3,current=4` bridge to serve a v3 UI.
No-overlap requests fail with HTTP 426 and headers advertising the bridge
range. Additive fields within v3 must have Serde defaults so older v3 payloads
remain readable. Renames retain a deserialization alias. Removing a field,
changing its meaning or type, or changing an SSE event name is incompatible
and advances the current version; the minimum advances only when older
versions are no longer served.

On upgrade, the UI recognizes legacy port/token-only discovery and health
responses without a negotiated-version echo. It performs one bounded
`systemctl --user restart` cycle (falling back to the existing start behavior
when restart is unavailable), then polls without restarting again. That
upgrade restart is claimed at most once for the UI process lifetime, so its
periodic reconnect cannot form a restart loop. A healthy manually launched
compatible bridge is left untouched; ordinary transport unavailability keeps
the prior non-disruptive `start` behavior.

## Protocol coverage

| HTTP surface | Shared contract |
| --- | --- |
| `GET /api/health` | Plain-text `ok`; version is negotiated in headers |
| `POST /api/chat` | `ChatRequest`; typed SSE events below |
| `POST /api/chat/:task_id/stream` | Replays the existing owner-scoped Job as typed SSE without resubmitting or cancelling on disconnect |
| `POST /api/chat/:task_id/follow-up` | `ChatRequest` → `TaskStarted`; derives the canonical session from the owner-checked predecessor and persists `after_task_id` |
| `POST /api/chat/:task_id/cancel` | `CancelResponse` / `ErrorEnvelope` |
| `GET /api/activities?state=…&limit=…` | `ActivityListQuery` → `ActivityListResponse`; `activity.list` |
| `POST /api/activities` | `ActivityCreateRequest` → `ActivityView`; `activity.create` |
| `GET /api/activities/:id` | `ActivityDetailResponse`; `activity.get` plus associated `permission.pending` projections |
| `GET /api/activities/:id/continuity/export` | Exact bounded `ActivityContinuityDocument`; `activity.continuity.export` |
| `POST /api/activities/continuity/import` | `ActivityContinuityImportRequest` → validated paused `ActivityContinuityImportAcknowledgement`; `activity.continuity.import` |
| `GET /api/activities/:id/receipts?limit=…` | `ActivityReceiptsQuery` → schema-1 `ActivityReceiptsResponse`; read-only `activity.receipts` |
| `GET /api/activities/:id/capability-policy` | Required-nullable schema-1 `ActivityCapabilityPolicyResponse`; `activity.capability_policy.get` |
| `POST /api/activities/:id/capability-policy` | `ActivityCapabilityPolicySetRequest` → `ActivityCapabilityPolicy`; CAS `activity.capability_policy.set` |
| `POST /api/activities/:id/capability-policy/enabled` | `ActivityCapabilityPolicyEnabledRequest` → `ActivityCapabilityPolicy`; `activity.capability_policy.enabled` |
| `GET /api/activities/:id/execution-limits` | Required-nullable schema-1 `ActivityExecutionLimitsResponse`; `activity.execution_limits.get` |
| `POST /api/activities/:id/execution-limits` | `ActivityExecutionLimitsSetRequest` → `ActivityExecutionLimits`; CAS `activity.execution_limits.set` |
| `POST /api/activities/:id/execution-limits/enabled` | `ActivityExecutionLimitsEnabledRequest` → `ActivityExecutionLimits`; explicit `activity.execution_limits.enabled` |
| `GET /api/activities/:id/monetary-budget` | Required-nullable schema-1 `ActivityMonetaryBudgetResponse`; `activity.monetary_budget.get` |
| `POST /api/activities/:id/monetary-budget` | `ActivityMonetaryBudgetSetRequest` → `ActivityMonetaryBudget`; CAS `activity.monetary_budget.set` |
| `POST /api/activities/:id/monetary-budget/enabled` | `ActivityMonetaryBudgetEnabledRequest` → `ActivityMonetaryBudget`; exact-revision `activity.monetary_budget.enabled` |
| `GET /api/activities/:id/scheduling-priority` | Required-nullable schema-1 `ActivitySchedulingPriorityResponse`; `activity.scheduling_policy.get` |
| `POST /api/activities/:id/scheduling-priority` | `ActivitySchedulingPrioritySetRequest` → `ActivitySchedulingPolicy`; CAS `activity.scheduling_policy.set` |
| `GET /api/activities/:id/object-state?reference=…&limit=…` | `ActivityObjectStateQuery` → schema-1 `ActivityObjectStateResponse`; `activity.object_state.list` |
| `POST /api/activities/:id/object-state` | `ActivityObjectStateRecordRequest` (`entry: ObjectStateDraft`) → `ObjectStateEntry`; append-only `activity.object_state.record` |
| `GET /api/activities/:id/objects` | `ActivityObjectsResponse`; declaration-only `activity.objects` |
| `POST /api/activities/:id/objects` | `ActivityObjectAttachRequest` → unchanged `ActivityView`; `activity.object.attach` |
| `POST /api/activities/:id/operation-preview` | `ActivityOperationPreviewRequest` → schema-1 metadata-only `ActivityOperationPreview`; `activity.operation.preview` |
| `PATCH /api/activities/:id` | `ActivityUpdateRequest` → `ActivityView`; `activity.update` |
| `POST /api/activities/:id/transition` | `ActivityTransitionRequest` → `ActivityView`; `activity.transition` |
| `POST /api/activities/:id/run` | `ActivityRunRequest` → `ActivityWorkResponse`; durable `activity.run` |
| `POST /api/tasks/:task_id/retry` | `ActivityWorkResponse`; `task.retry` |
| `GET /api/sessions` | `Vec<SessionSummary>` / `ErrorEnvelope` |
| `GET /api/sessions/:id` | `SessionSummary` / `ErrorEnvelope` |
| `DELETE /api/sessions/:id` | `ErrorEnvelope` (not implemented) |
| `GET /api/sessions/:id/history` | `HistoryResponse` / `ErrorEnvelope` |
| `GET /api/models` | `ModelsResponse` / `ErrorEnvelope` |
| `POST /api/voice/upload` | Raw audio request; `VoiceResponse` / `ErrorEnvelope` |

The chat stream covers `task`, `delta` (`text` remains a decode alias),
`tool_use_start`, `tool_use`, `tool_start`, `tool_result`, `warning`,
`turn_done`, `done`, and `error`. The shared decoder also retains the
`tool_input_delta` compatibility event, while the bridge continues suppressing
live tool arguments. Unknown future event names are ignored by v3 clients;
malformed known events fail decoding.

Activity endpoints use the same bearer authentication, v3 negotiation, and
typed error envelopes as chat. Request DTOs accept no owner or capability
fields: clawd derives ownership from the bridge's kernel identity. The
translation module validates the broker's Activity schema and removes private
job fields before emitting presentation DTOs. Object-state entries retain the
contract's typed server-supplied `owner_uid`; it is never a caller selector.
Execution-limit and capability-policy replies also retain it and are checked against the same
process-identity helper used for private bridge discovery. Other Activity views omit owner internals. Additive job/session/approval fields
retain their defaults within presentation protocol v3. Continuity remains
intentionally non-additive: its complete shape is exact and unknown fields fail.

## License

This subtree is licensed Apache-2.0, same as the rest of ClawOS.

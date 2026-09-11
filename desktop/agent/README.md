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
│           ├── sessions.rs # GET/DELETE /api/sessions[/:id]
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
version is independent of the desktop HTTP/SSE presentation protocol v1.
The additive Activity routes leave both versions unchanged.

The UI and bridge both compile against `protocol/` (`cos-agent-protocol`).
That crate exclusively owns the desktop presentation contract: endpoint DTOs,
named SSE payloads, stable error envelopes, discovery metadata, and protocol
version constants. It depends only on Serde and `serde_json`. The bridge
remains the anti-corruption layer: `bridge/src/translation.rs` decodes generic
clawd results, removes worker/task storage details and raw memory content, and
emits only protocol types. The UI does not deserialize clawd or core models.

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
usage, and final metadata. Stop cancels the clawd task; dropping the
client stream also triggers bridge-side cancellation.

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

## Endpoint discovery

The bridge binds an ephemeral port when `COS_AGENT_BRIDGE_PORT` is
unset (the systemd default), generates a random bearer token, and
atomically writes both values to
`$XDG_RUNTIME_DIR/cos-agent-bridge/endpoint.json` with mode `0600`.
The native UI and `cos app agent` launcher read this file and attach
the token to every bridge request.

## Protocol compatibility

Protocol v1 uses the `x-clawos-agent-protocol-version` request and response
header. Discovery also publishes `min_protocol_version` and
`protocol_version`. Missing, malformed, or unsupported versions fail with HTTP
426 and a typed
`incompatible_protocol_version` or `protocol_version_required` error; the UI
selects the highest version in the intersection of its compiled range and the
bridge discovery range. The bridge validates that selected version and echoes
it on every response; the UI rejects a missing or different echo.

The current binaries support exactly v1 (`min=1`, `current=1`), while the
intersection policy permits a future `min=1,current=2` bridge to serve a v1 UI.
No-overlap requests fail with HTTP 426 and headers advertising the bridge
range. Additive fields within v1 must have Serde defaults so older v1 payloads
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
| `POST /api/chat/:task_id/cancel` | `CancelResponse` / `ErrorEnvelope` |
| `GET /api/activities?state=…&limit=…` | `ActivityListQuery` → `ActivityListResponse`; `activity.list` |
| `POST /api/activities` | `ActivityCreateRequest` → `ActivityView`; `activity.create` |
| `GET /api/activities/:id` | `ActivityDetailResponse`; `activity.get` plus associated `permission.pending` projections |
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
live tool arguments. Unknown future event names are ignored by v1 clients;
malformed known events fail decoding.

Activity endpoints use the same bearer authentication, v1 negotiation, and
typed error envelopes as chat. Request DTOs accept no owner or capability
fields: clawd derives ownership from the bridge's kernel identity. The
translation module validates the broker's Activity schema and removes owner
internals and private job fields before emitting presentation DTOs. Additive
job/session/approval fields have defaults within presentation protocol v1.

## License

This subtree is licensed Apache-2.0, same as the rest of ClawOS.

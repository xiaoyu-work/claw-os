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
│       └── routes/
│           ├── chat.rs     # POST /api/chat   (SSE stream)
│           ├── sessions.rs # GET/DELETE /api/sessions[/:id]
│           ├── models.rs   # GET /api/models
│           └── voice.rs    # POST /api/voice/upload → configured STT provider
└── ui/                     # cos-agent-ui — native libcosmic chat
    ├── src/
    │   ├── main.rs         #   Application assembly, routing, subscription
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
           │  /api/*  (JSON + SSE)    │
           └────────────┬─────────────┘
                        │
                        ▼  Unix socket
           ┌──────────────────────────┐
           │  clawd                   │
           │  task.submit / stream    │
           └──────────────────────────┘
```

The bridge and approval applet share `crates/clawd-client` for canonical
`CLAWD_SOCKET` discovery (`COS_CLAWD_SOCKET` remains a compatibility alias),
v1 request IDs/envelopes, `CBK1` length-prefixed framing, deadlines, bounds,
and typed transport/protocol errors.

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
messages. Stream generations and cancellation remain centralized in
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
the configured `cos model transcribe` provider. App/window context is
handed to the UI through an anonymous pipe rather than argv, environment, or a
pathname. The host and UI fail closed
without strong Yama ptrace isolation and become non-dumpable. A readiness
handshake ensures the parent writes nothing until the UI is hardened. The new
process validates the typed activation and runs a dedicated transient overlay,
avoiding plaintext context on the unauthenticated single-instance D-Bus path.
It then sends context through an untrusted-data system boundary without
storing it as the visible user prompt.

The UI install recipe targets `/usr/local/bin/cos-agent-ui`; private context
launches accept no executable override or `PATH` lookup.

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

## License

This subtree is licensed Apache-2.0, same as the rest of ClawOS.

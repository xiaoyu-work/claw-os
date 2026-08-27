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
    │   ├── main.rs         #   Application impl, view, subscription
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

The bridge no longer serves a static SPA — the previous React
frontend was retired in favour of `cos-agent-ui`. Every UI surface
talks only to the `/api/*` endpoints.

The overlay is a single-instance Wayland layer-shell surface:

- `Super+A` opens the compact multiline summon composer.
- `Super+Shift+A` opens the live voice orb and begins recording.
- Re-invoking either shortcut reuses the existing overlay process.
- Escape stops/cancels active work before closing the surface.

Chat streams expose task identity, live text, tool lifecycle, warnings,
usage, and final metadata. Stop cancels the clawd task; dropping the
client stream also triggers bridge-side cancellation.

Voice uploads are staged as private runtime files and transcribed via
the configured `cos model transcribe` provider. App/window context is
sent through a transient, untrusted-data system boundary and is not
stored as the visible user prompt.

## Endpoint discovery

The bridge binds an ephemeral port when `COS_AGENT_BRIDGE_PORT` is
unset (the systemd default), generates a random bearer token, and
atomically writes both values to
`$XDG_RUNTIME_DIR/cos-agent-bridge/endpoint.json` with mode `0600`.
The native UI and `cos app agent` launcher read this file and attach
the token to every bridge request.

## License

This subtree is licensed Apache-2.0, same as the rest of ClawOS.

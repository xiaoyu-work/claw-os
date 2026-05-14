# ClawOS Agent App (`com.clawos.Agent`)

The user-facing face of the ClawOS system Agent. The brain (LLM
runtime, providers, tools, caps, memory) lives in `core/src/agent/`
and is reachable via the `cos agent` CLI. This directory holds the
**desktop UI surface** plus the small local HTTP bridge that
brokers a streaming JSON+SSE protocol between the UI and the CLI.

## Layout

```
desktop/agent/
├── Cargo.toml              # workspace: bridge + ui
├── docs/
│   └── design-system.md    # shared dark-surface / emerald-accent system
├── bridge/                 # cos-agent-bridge — HTTP+SSE daemon
│   └── src/
│       ├── main.rs         # 127.0.0.1 Axum server (/api only)
│       ├── state.rs        # port discovery + cos binary location
│       └── routes/
│           ├── chat.rs     # POST /api/chat   (SSE stream)
│           ├── sessions.rs # GET/DELETE /api/sessions[/:id]
│           ├── models.rs   # GET /api/models
│           └── voice.rs    # POST /api/voice/upload
└── ui/                     # cos-agent-ui — native libcosmic chat
    ├── src/
    │   ├── main.rs         #   Application impl, view, subscription
    │   ├── bridge.rs       #   port discovery + wire types
    │   ├── sse.rs          #   reqwest-based SSE consumer
    │   └── recorder.rs     #   cpal mic capture + WAV upload
    └── assets/             #   brand PNGs baked into binary
```

## Runtime topology

```
┌─ standalone window ─┐   ┌─ Super+A overlay ─┐
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
                        ▼  subprocess
           ┌──────────────────────────┐
           │  cos agent stream "..."  │
           │  core::agent::runtime    │
           └──────────────────────────┘
```

The bridge no longer serves a static SPA — the previous React
frontend was retired in favour of `cos-agent-ui`. Every UI surface
talks only to the `/api/*` endpoints.

## Port discovery

The bridge binds an ephemeral port when `COS_AGENT_BRIDGE_PORT` is
unset (the systemd default) and writes the bound port to
`$XDG_RUNTIME_DIR/cos-agent-bridge.port`. The native UI and the
`cos app agent` launcher both read this file to discover the URL.

## License

This subtree is licensed Apache-2.0, same as the rest of ClawOS.

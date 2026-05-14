# ClawOS Agent App (`com.clawos.Agent`)

The user-facing face of the ClawOS system Agent. The brain (LLM
runtime, providers, tools, caps, memory) lives in `core/src/agent/`
and is reachable via the `cos agent` CLI. This directory holds the
**desktop UI surface** plus the small local HTTP bridge that lets a
React UI talk to that CLI.

## Layout

```
desktop/agent/
├── Cargo.toml              # workspace: bridge + overlay + ui
├── bridge/                 # cos-agent-bridge — HTTP+SSE daemon
│   └── src/
│       ├── main.rs         # 127.0.0.1 Axum server
│       ├── state.rs        # port discovery, web root, cos binary
│       └── routes/
│           ├── chat.rs     # POST /api/chat   (SSE stream)
│           ├── sessions.rs # GET/DELETE /api/sessions[/:id]
│           ├── models.rs   # GET /api/models
│           └── voice.rs    # POST /api/voice/upload
├── overlay/                # cos-agent-overlay — Super+A summon window
│   └── src/main.rs         # layer-shell quick chat (iced/libcosmic)
├── ui/                     # cos-agent-ui — native libcosmic chat
│   ├── src/                #   (replaces web/ + cos-browser surface)
│   │   ├── main.rs         #   Application impl, view, subscription
│   │   ├── bridge.rs       #   port discovery + wire types
│   │   └── sse.rs          #   reqwest-based SSE consumer
│   └── assets/             #   brand PNGs baked into binary
└── web/                    # legacy: vendored open-agents Next.js UI
                            #   (kept while ui/ is stabilized)
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
           │  - /api/*  (JSON+SSE)    │
           │  - /       (static SPA)  │
           └────────────┬─────────────┘
                        │
                        ▼  subprocess
           ┌──────────────────────────┐
           │  cos agent stream "..."  │
           │  core::agent::runtime    │
           └──────────────────────────┘
```

> Legacy: `cos-browser` loading the static React app under
> `web/` still works against the same bridge. It will be
> retired once `cos-agent-ui` reaches feature parity (markdown,
> tool cards, voice, settings).

## Port discovery

The bridge binds an ephemeral port when `COS_AGENT_BRIDGE_PORT` is
unset (the systemd default) and writes the bound port to
`$XDG_RUNTIME_DIR/cos-agent-bridge.port`. The overlay and the
`cos app agent` launcher both read this file to discover the URL.

## License

This subtree is licensed Apache-2.0, same as the rest of ClawOS.
The vendored UI in `web/` was originally MIT-licensed
(Vercel `open-agents`, Copyright 2026 Vercel, Inc.) — see
`web/LICENSE` and the project-root `NOTICE` for attribution.

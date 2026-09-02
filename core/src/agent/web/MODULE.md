# Agent Web Module

## Purpose

`web/` exposes the authenticated local agent HTTP/SSE API and serves the agent
web UI.

## Responsibilities

- Authenticate local browser/desktop requests.
- Map HTTP routes to agent/session/setup operations.
- Stream text, tools, reasoning presentation, usage, and terminal state.
- Subscribe to owner-scoped notifications and expose live unread,
  acknowledgement, dismissal, and delivery-preference UI.
- Reuse frozen session prompts and configured history compression through the
  shared runtime.
- Serve built UI assets without exposing credentials.

## Key Files

| Path | Role |
| --- | --- |
| `routes/` | HTTP and SSE endpoint handlers |
| `routes/notifications.rs` | Notification list, SSE, state, and preference bridge to `clawd` |
| `ui/` | TypeScript/React source and generated distribution assets |
| `mod.rs` | Server/router assembly |

## Dependencies

Routes call agent/core services and use the same guarded runtime paths as CLI
requests. SSE presentation is not conversation authority or persisted provider
state. Generated `ui/dist/` assets come from the UI build.

## Tests

Run relevant Rust route tests and, for UI changes, the existing UI package
build/test commands from `ui/`.

```bash
cargo test -p cos agent::web:: -- --test-threads=1
```

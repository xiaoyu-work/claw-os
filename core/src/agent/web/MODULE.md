# Agent Web Module

## Purpose

`web/` exposes the authenticated local agent HTTP/SSE API and serves the agent
web UI.

## Responsibilities

- Authenticate local browser/desktop requests.
- Map HTTP routes to agent/session/setup operations.
- Present the shared Activity broker's goals, planning metadata, explicit state
  changes, and associated job/session views without a Web-owned store or lifecycle.
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
| `routes/activities.rs` | Activity HTTP adapters using the broker's bounded request DTOs and existing authenticated transport |
| `ui/` | TypeScript/React source and generated distribution assets |
| `ui/src/pages/activities.tsx`, `ui/src/components/activity-*.tsx` | Fixed Activity list, planning forms, detail cards, and existing task/approval/session navigation |
| `ui/src/lib/activities.ts`, `ui/src/hooks/use-activities.ts` | Validated fetched views and abortable, selection-safe refreshes |
| `mod.rs`, `server.rs` | Serve command and authenticated router assembly |

## Dependencies

Routes call agent/core services and use the same guarded runtime paths as CLI
requests. SSE presentation is not conversation authority or persisted provider
state. Generated `ui/dist/` assets come from the UI build.
Activities always cross `clawd`; they have no private fallback if a read or
mutation fails. Resources are inert text, boundaries do not grant permissions,
and successful jobs never imply completed goals. The shared contract is in
[`docs/activities.md`](../../../../docs/activities.md).

## Tests

Run relevant Rust route tests and, for UI changes, the existing UI package
build/test commands from `ui/`.

```bash
cargo test -p cos agent::web:: -- --test-threads=1
cargo test -p cos agent::web::routes::activities::tests -- --test-threads=1
```

Activity adapter tests cover authentication, bounded/closed request decoding,
explicit confirmation forwarding, broker transport, and visible failures.
[`ui/README.md`](ui/README.md) documents focused UI tests and a real Chromium
workflow over a mocked authenticated API, including stale selection responses.

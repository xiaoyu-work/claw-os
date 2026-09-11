# Agent Web Module

## Purpose

`web/` exposes the authenticated local agent HTTP/SSE API and serves the agent
web UI.

## Responsibilities

- Authenticate local browser/desktop requests.
- Map HTTP routes to agent/session/setup operations.
- Present the shared Activity broker's goals, planning metadata, explicit state
  changes, and associated job/session views without a Web-owned store or lifecycle.
- Attach typed App object references and display the broker's authenticated
  declaration metadata and diagnostics, without URI parsing or App execution.
- Preview App-declared effects on explicit request, keeping requested targets,
  recovery guidance, unresolved arguments, and authorization caveats distinct
  from execution results.
- Read immutable caller-reported receipts, keeping report outcomes and result
  summaries separate from matched App declarations and OS-confirmed evidence.
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
| `routes/activities.rs` | Activity and object-reference HTTP adapters using the broker's bounded request DTOs and existing authenticated transport |
| `ui/` | TypeScript/React source and generated distribution assets |
| `ui/src/pages/activities.tsx`, `ui/src/components/activity-*.tsx` | Fixed Activity list, planning forms, detail cards, and existing task/approval/session navigation |
| `ui/src/lib/activities.ts`, `ui/src/hooks/use-activities.ts` | Validated fetched views and abortable, selection-safe refreshes |
| `ui/src/components/activity-objects.tsx` | Typed object attachment, declaration-only status, inert references, and structured invocation metadata |
| `ui/src/components/operation-effects.tsx`, `ui/src/lib/operation-preview.ts` | Per-object metadata-only effect previews and strict response validation |
| `ui/src/components/activity-receipts.tsx`, `ui/src/lib/activity-receipts.ts` | Read-only caller-reported receipts, source/identity checks, inert summaries, and declaration diagnostics |
| `ui/src/lib/api-shapes.ts` | Shared response-shape guards used by Activity and preview adapters |
| `mod.rs`, `server.rs` | Serve command and authenticated router assembly |

## Dependencies

Routes call agent/core services and use the same guarded runtime paths as CLI
requests. SSE presentation is not conversation authority or persisted provider
state. Generated `ui/dist/` assets come from the UI build.
Activities always cross `clawd`; they have no private fallback if a read or
mutation fails. Resources are inert text, boundaries do not grant permissions,
and successful jobs never imply completed goals. The shared contract is in
[`docs/activities.md`](../../../../docs/activities.md).
Object attachments use `activity.object.attach`, not a client-side replacement
of the resource list. `activity.objects` supplies descriptions only after App
verification; `declared` does not prove data existence, freshness, or permission.
Invalid/unavailable references retain their diagnostics. Malformed responses
hide stale descriptions instead of becoming local object truth. See
[`docs/app-objects.md`](../../../../docs/app-objects.md).
`activity.operation.preview` uses the same broker service as terminal/native
clients. It receives structured argv, never a runnable shell string. Preview
state is transient and scoped to the Activity, object, package version, and
invocation; metadata refreshes do not automatically request an effect preview.
Neither effect kinds nor targets are inferred locally. Responses claiming
execution, checked authorization, or confirmed effects are rejected. The
meaning of declarations and recovery labels is defined in
[`docs/operation-previews.md`](../../../../docs/operation-previews.md).
`activity.receipts` is a read-only owner-scoped projection of the shared ledger.
The Web receipt surface cannot author, edit, replay, or delete receipts. It accepts only
`caller_reported` provenance, checks Activity/owner identity, and never uses a
report outcome to complete a goal or grant authority. Stored declaration
snapshots authenticate matching App metadata at recording time, not current App
validity after changes/revocation or the reported execution. Recording time and
reported original-output digests are explicitly distinguished from execution
time and OS evidence; result previews and errors stay inert text. See
[`docs/execution-receipts.md`](../../../../docs/execution-receipts.md).

## Tests

Run relevant Rust route tests and, for UI changes, the existing UI package
build/test commands from `ui/`.

```bash
cargo test -p cos agent::web:: -- --test-threads=1
cargo test -p cos agent::web::routes::activities::tests -- --test-threads=1
```

Activity adapter tests cover authentication, bounded/closed request decoding,
explicit confirmation forwarding, typed object attachment, broker transport,
and visible failures.
[`ui/README.md`](ui/README.md) documents focused UI tests and a real Chromium
workflow over a mocked authenticated API, including stale selection responses,
canonical object attachments, declaration failures, non-execution, unknown
effects, requested-only targets, isolated late previews, receipt provenance,
uncertain/error reports, declaration failures, and terminal-state receipt reads.

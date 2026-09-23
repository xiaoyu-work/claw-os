# Agent Web Module

## Purpose

`web/` exposes the authenticated local agent HTTP/SSE API and serves the agent
web UI.

## Responsibilities

- Authenticate local browser/desktop requests.
- Map HTTP routes to agent/session/setup operations.
- Present owner-scoped conversation inventory, metadata, history and
  presentation mutations through `agent.conversation.*`; Web never opens or
  mutates Agent memory as a separate authority.
- Present the shared Activity broker's goals, planning metadata, explicit state
  changes, and associated job/session views without a Web-owned store or lifecycle.
- Export and import the broker's closed portable Activity continuity document
  with owner identity derived from authentication and explicit local placement.
- Attach typed App object references and display the broker's authenticated
  declaration metadata and diagnostics, without URI parsing or App execution.
- Preview App-declared effects on explicit request, keeping requested targets,
  recovery guidance, unresolved arguments, and authorization caveats distinct
  from execution results.
- Read immutable caller-reported receipts, keeping report outcomes and result
  summaries separate from matched App declarations and OS-confirmed evidence.
- Stream text, tools, reasoning presentation, usage, and terminal state.
- Present generic tool lifecycle, latency and result size; only failed tools
  may expose a bounded, redacted error preview, never successful result bodies.
- Reattach Chat to an owner-scoped durable task stream after navigation or
  reload, using verified conversation task bindings to avoid replay duplicates.
- Queue busy-time Chat follow-ups as ordinary durable Jobs linked by
  `after_task_id`; browser component state is never the queue authority.
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
| `ui/src/components/activity-object-state.tsx`, `ui/src/lib/object-state.ts` | Shared object-state forms/history, source/validity caveats and selection-safe correction/retraction |
| `ui/src/components/activity-execution-limits.tsx`, `ui/src/lib/execution-limits.ts` | Revision-checked finite execution controls with backend-owned counters; no permission or goal-state authority |
| `ui/src/components/activity-monetary-budget.tsx`, `ui/src/lib/monetary-budget.ts` | Exact-revision configured-accounting controls with decimal-string micro-USD values; no pricing, invoice, ledger or policy authority |
| `ui/src/components/activity-continuity.tsx`, `ui/src/lib/activity-continuity.ts` | Strict continuity-v1 parsing, deterministic selected-Activity download, and bounded explicit local import that creates a paused Activity |
| `ui/src/components/activity-capability-policy.tsx`, `ui/src/lib/capability-policy.ts` | Fixed typed Normal/Ask/Deny rules, strict owner/revision/scope acknowledgements and retained stale drafts; no local capability authority |
| `ui/src/lib/api-shapes.ts` | Shared response-shape guards used by Activity and preview adapters |
| `mod.rs`, `server.rs` | Serve command and authenticated router assembly |

## Dependencies

Routes call agent/core services and use the same guarded runtime paths as CLI
requests. SSE presentation is not conversation authority or persisted provider
state. Generated `ui/dist/` assets come from the UI build.
Conversation list/detail/history/update/fork routes adapt the shared broker
contract used by terminal and native presentations. Canonical session identity,
owner-memory access, idle-session mutation locking, archive state and branch
lineage remain broker-owned; browser search is only a filter over the returned
bounded inventory. Older memory-only sessions remain discoverable through the
read-only `memory.*` broker routes and are visibly non-manageable.
Conversation history projects bounded Job metadata from the same broker view.
When its task bindings are complete, Chat keeps the active outer user prompt,
removes that task's already-persisted intermediate presentation rows, and
replays the owner-checked task stream from a cursor. Incomplete bindings remain
visible as an explicit reconstruction error rather than being guessed from
text or timestamps.
Tool progress reaches Web only after the runtime's user-visible projection has
removed inputs and successful result bodies. The durable stream retains
identity, status, latency, byte count and an optional bounded/redacted failure
preview; the Web route redacts and bounds that preview again before SSE.
Busy-time follow-ups derive the canonical conversation from an owner-checked
predecessor Job, persist through `task.submit.after_task_id`, and chain each
later follow-up after the previously returned Job. Chat then discovers and
attaches the next runnable Job through the same conversation projection.
Activity adapters retain Main's authenticated owner transport and local/remote
Web session provenance. Task submission, safe retry, App-service admission and
protected owner App review remain backend responsibilities. The existing
capability-approval Web page is not a replacement for `cos review` or the native
OS review presenter, and Activity text remains labelled untrusted owner data.
Main's protocol-11 worker keeps PREPARE/COMMIT admission and bound approvals;
dynamic App/MCP execution stays in the isolated extension Host. Web adapters
do not restore retired worker AppHost control or weaken nonce/digest/context
binding when presenting Activity work.
Activities always cross `clawd`; they have no private fallback if a read or
mutation fails. Resources are inert text, boundaries do not grant permissions,
and successful jobs never imply completed goals. The shared contract is in
[`docs/activities.md`](../../../../docs/activities.md).
Activity monetary-budget routes use the same owner-scoped broker service as
terminal and native clients. The Web DTO uses decimal strings for revisions and
micro-USD values that may exceed JavaScript's safe integer range, and labels
rates/totals as configured accounting rather than provider billing. See
[`docs/activity-monetary-budgets.md`](../../../../docs/activity-monetary-budgets.md).
Activity continuity routes forward only `activity.continuity.export/import`.
The server revalidates the complete broker document and import acknowledgement,
projects lineage revisions as decimal strings, accepts no owner or machine
selector, and permits only explicit `local` placement. The browser performs a
bounded duplicate-aware parse before presentation or forwarding, downloads a
deterministic canonical JSON Blob, and selects the imported Activity only after
an exact paused/new/local acknowledgement. Continuity carries intent, semantic
references, finite safe rules, and scheduling preference only; it is not sync,
backup, authority, consent, completion, result, or execution proof. See
[`docs/activities.md`](../../../../docs/activities.md#portable-continuity).
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
continuity document/acknowledgement revalidation, and visible failures.
[`ui/README.md`](ui/README.md) documents focused UI tests and a real Chromium
workflow over a mocked authenticated API, including stale selection responses,
canonical object attachments, declaration failures, non-execution, unknown
effects, requested-only targets, isolated late previews, receipt provenance,
uncertain/error reports, declaration failures, and terminal-state receipt reads.

Object state uses `activity.object_state.list/record`, with immutable drafts and
server-derived owner/source/time. Its author classifications are caller reports,
never verified facts; linked App output is projected from an existing receipt.
The Web holds no authoritative copy, does not resolve an object, and never
turns a relationship into scheduled work. See
[`docs/object-state.md`](../../../../docs/object-state.md).

Capability policy uses `activity.capability_policy.get/set/enabled` and the
same bounded broker DTOs as terminal/native clients. The authenticated
`/api/activities/capability-policy-catalog` projects only `caps::CATALOG`;
it performs no App discovery, filesystem or credential I/O. Forms never
evaluate permissions or coverage locally. Failed writes retain the draft and
require explicit refresh; newer revisions need an explicit review action
before the draft can be saved again. See
[`docs/activity-capability-policies.md`](../../../../docs/activity-capability-policies.md).

Focused `ui/test/capability-policy*.test.*` tests cover closed policy responses,
typed scope compatibility, bounded drafts, owner/revision checks, inert
rendering, terminal controls and stale edits. The existing Chromium harness
exercises real rule edits, persistence, HTTP failures, explicit conflict
recovery and selection-safe late requests; it does not substitute HTTP 200
checks for completed user operations.

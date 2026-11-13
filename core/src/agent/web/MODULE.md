# Agent Web Module

## Purpose

`web/` exposes the authenticated local agent HTTP/SSE API and serves the agent
web UI.

## Responsibilities

- Authenticate local browser/desktop requests.
- Open the Windows default browser for `--open` under WSL rather than a WSLg
  Linux browser, so host input methods remain available. Bind the listener
  before launch and report failed interop without silently switching browsers.
- Present the broker-owned Memory center, including source/revision/time,
  checked notes, explicit App forgetting and read-only conversation/index views.
- Present task-linked capability reviews inline through the protected shared
  OS review DTO, preserving displayed revisions and controller-supported
  choices; App confirmation remains in the OS review presenter.
  Chat, Tasks and Approvals share compact, expandable rows. Resolved capability
  consent is labelled Authorized/Denied, never task completion. Only pending
  unsupported choices become blocking diagnostics; real status errors remain visible.
- Present separate package-impact/all-App-stop reviews in top-level OS
  Approvals, not App Store installation or capability rows. Root supplies the
  plan, impact, uncertainty disclosures, current actions and displayed
  revision. Global-stop confirmation only starts the existing process-bound
  polkit/PAM dialog; owner cancellation uses `system.review.cancel`.
  Confirmation recorded/used is never package-transaction success. Handled
  reviews are refreshed from Root rather than inferred from queue absence.
- Present OS password authentication in a bounded in-page dialog through a
  process-bound polkit agent, including WSL without a desktop/logind session.
  Bind responses to the owner, Web token, origin, displayed review and current
  single-use prompt. Passwords go only to the OS verifier, never to the Agent,
  credential store, logs or conversation state.
  The serving process disables core dumps and owner-level memory inspection
  before an OS authentication conversation starts.
- Map HTTP routes to agent/session/setup operations.
- Report the running binary's compiled build version through `/api/meta`,
  sharing system information's source instead of a runtime environment value.
- Keep setup pages available without a configured model, but reject a new
  Chat submission with an actionable Settings/Text diagnostic before creating
  a Job when the shared provider/credential readiness check fails.
- Present owner-scoped conversation inventory, metadata, history and
  presentation mutations through `agent.conversation.*`; Web never opens or
  mutates Agent memory as a separate authority.
- Present explicit Activity automatic-work configuration, scheduler-authorized
  start/stop and inspected resumption. Revisions use decimal strings; handoffs
  remain caller reports and the browser owns no loop or durable execution state.
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
- Keep provider preparation distinct from runtime dispatch and unconfirmed
  outcomes; display bounded evidence provenance without treating it as effect
  verification or goal completion.
- Present generic tool lifecycle, latency and result size; only failed tools
  may expose a bounded, redacted error preview, never successful result bodies.
- Reattach Chat to an owner-scoped durable task stream after navigation or
  reload, using verified conversation task bindings to avoid replay duplicates.
- Recover transient stream loss through bounded, abortable same-Job replay,
  including explicit detection of EOF before a terminal event; never retry
  submission when its outcome is unknown.
- Queue busy-time Chat follow-ups as ordinary durable Jobs linked by
  `after_task_id`; browser component state is never the queue authority.
- Show bounded per-Job follow-up previews and explicit individual/selected
  cancellations separately from stopping current work. Selected cancellation
  snapshots exclude the current task and later queue arrivals.
- Accept bounded image selection, paste and drop, then submit the same
  broker-verified Job attachment contract used by native Desktop.
- Subscribe to owner-scoped notifications and expose live unread,
  acknowledgement, dismissal, and delivery-preference UI.
- Keep per-notification read actions directly visible and route Mark all read
  through the shared owner-wide atomic service, not a loop over displayed cards.
- Report the visible focused conversation through `notification.view` and
  renew its short lease. Initial Chat/follow-up submission carries the same
  viewer revision before queue publication. Hiding, leaving or closing clears
  the hint without cancelling work; it never supplies attended authorization.
- Reuse frozen session prompts and configured history compression through the
  shared runtime.
- Serve built UI assets without exposing credentials.
- Provide the official App Store, retained installation/review status and
  installed App navigation without a model request. Catalogs and immutable
  snapshots come only from `xiaoyu-work/clawos-app`; `clawd` verifies freshness,
  digests and publisher provenance and publishes only after protected owner
  confirmation.
  Individual and bundled APT App reviews retain full-width display revisions
  as decimal strings; package-impact decisions remain in OS Approvals.
- Host each product's signed `claw.web-app/v1` resources in a script-enabled
  sandbox without same-origin access. View-bound nonces relay only the owning
  App's declared MCP tools through the existing CLI App-service path, including
  an exact package binding. App frames receive no Agent Web token or task API.
- Keep Copilot authorization and model configuration as separate visible
  stages. Only pending/slow-down responses continue polling; failures remain
  visible after refresh, and cancellation prevents late configuration writes.

## Key Files

| Path | Role |
| --- | --- |
| `routes/` | HTTP and SSE endpoint handlers |
| `routes/workspaces.rs`, `ui/src/lib/workspaces.ts`, `ui/src/pages/workspaces.tsx` | Shared owner Workspace CRUD, source/search/index and temporary-read controls with strict bounded decoding and decimal-string revisions |
| `routes/memory.rs`, `ui/src/pages/memory.tsx` | Same-owner memory inspection, checked learned-note editing and explicit forgetting through the shared broker |
| `routes/approvals/presentation.rs`, `ui/src/components/chat-approvals.tsx` | Shared readable capability-review projection, same-task discovery, exact displayed-revision decisions and helper delegation |
| `routes/approvals/{presentation,authorization}.rs` | Separate Root package-impact projection, revision-only authorization start and non-grant cancellation; never an App-install confirmation |
| `authorization.rs`, `authorization/polkit.rs`, `routes/approvals/authorization.rs` | Ephemeral browser-bound polkit/PAM conversation, fixed OS helpers, deadlines/cancellation and root-broker outcome reconciliation |
| `ui/src/components/os-authorization.tsx` | Password-only dialog outside untrusted Markdown; no secret React state, storage or automatic response replay |
| `routes/notifications.rs` | Notification list, SSE, state, preferences and owner-scoped conversation-view bridge to `clawd` |
| `routes/activities.rs` | Activity and object-reference HTTP adapters using the broker's bounded request DTOs and existing authenticated transport |
| `routes/activities/work.rs`, `ui/src/components/activity-auto-work.tsx` | Shared automatic-work controls and bounded reported handoffs, without local scheduling or permission authority |
| `ui/` | TypeScript/React source and generated distribution assets |
| `ui/src/components/cos-sidebar.tsx` | New chat and conversation history alongside OS navigation, without duplicate Chat or primary Tasks entries |
| `ui/src/pages/tasks.tsx` | Contextual global execution inspection and stop/retry controls through existing Chat/Activity links and `/tasks`; not a primary sidebar entry |
| `ui/src/pages/notifications.tsx`, `ui/src/components/notification-settings.tsx` | Notification-first bounded Inbox, compact browser controls and grouped delivery-settings dialog; original permission and preference contracts remain unchanged |
| `ui/src/lib/chat-stream.ts` | Terminal-event validation and bounded same-Job transport recovery through verified history replay |
| `ui/src/lib/conversation-view.ts` | Focus/visibility leases, initial submission viewer metadata and detach-only cleanup; no consent, cancellation or replay |
| `ui/src/lib/chat-presentation.ts`, `ui/src/components/chat-evidence.tsx` | Truthful tool-phase reduction and inert evidence/source presentation |
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
| `routes/apps.rs`, `app_ui.rs` | Store adapters, protected App confirmation, view-bound signed assets and same-App MCP mediation |
| `ui/src/pages/app-store.tsx`, `ui/src/pages/apps.tsx`, `ui/src/pages/app.tsx` | Official releases, installed identities and isolated product-owned interfaces |

## Dependencies

Routes call agent/core services and use the same guarded runtime paths as CLI
requests. SSE presentation is not conversation authority or persisted provider
state. Generated `ui/dist/` assets come from the UI build.
Positive Web review actions register an authentication agent for the serving
PID/start time, not a desktop session or fallback agent. Only the authenticated
polkit service can start the fixed approval action, and only the serving
account's offered identity is accepted. OS-password responses use the standard
polkit verifier socket/SUID helper; they never replace polkit's proof or the
Root helper's protected review validation. `pkexec`'s internal terminal agent
is disabled. Failed, cancelled and uncertain outcomes require a broker refresh,
never automatic grant replay. Auth sessions are process-local and expire; no
new daemon, portable authority or desktop runtime dependency is introduced.

The installed-App browser fixture separately bounds one cold hosted-Chromium
boot at 30 monotonic seconds, before any App navigation or tool dispatch.
It retains bounded process/page-fault diagnostics on failure and never
restarts or prewarms the browser. App/CDP action, service, authority, native
first-frame and package-effect deadlines are unchanged; browser boot is not
an App outcome or a performance claim.

Conversation list/detail/history/update/fork routes adapt the shared broker
contract used by terminal and native presentations. Canonical session identity,
owner-memory access, idle-session mutation locking, archive state and branch
lineage remain broker-owned; browser search is only a filter over the returned
bounded inventory. Older memory-only sessions remain discoverable through the
read-only `memory.*` broker routes and are visibly non-manageable.
Conversation history projects bounded Job metadata from the same broker view.
Verified Jobs include bounded retained reasoning, cumulative usage, tool
identity/outcome/metrics and image metadata. Chat folds each snapshot into its
exact task-bound rows once; live tasks replay instead of adding the snapshot
again. Missing or over-budget details remain explicit, and raw images,
opaque provider state and successful tool bodies are not part of this view.
When its task bindings are complete, Chat keeps the active outer user prompt,
removes that task's already-persisted intermediate presentation rows, and
replays the owner-checked task stream from a cursor. Incomplete bindings remain
visible as an explicit reconstruction error rather than being guessed from
text or timestamps.
`ui/src/lib/chat-stream.ts` owns bounded Web transport recovery. Each retry
fetches verified bindings and replaces partial presentation before replay from
zero, so text, usage and tool events cannot be double-counted. A missing task
identity, incompatible binding or exhausted retry budget remains explicit;
manual reconnect and Tasks stay available without automatic resubmission.
Tool progress reaches Web only after the runtime's user-visible projection has
removed inputs and successful result bodies. The durable stream retains
identity, status, latency, byte count and an optional bounded/redacted failure
preview; the Web route redacts and bounds that preview again before SSE.
Busy-time follow-ups derive the canonical conversation from an owner-checked
predecessor Job, persist through `task.submit.after_task_id`, and chain each
later follow-up after the previously returned Job. Chat then discovers and
attaches the next runnable Job through the same conversation projection.
The browser keeps selected image bytes only in composer state and the
authenticated request.
Up to four originals may total 20 MiB decoded, within 32-MiB Chat/follow-up
HTTP frames. Concurrent reads are serialized and navigation rejects late file
reads; no resizing or lossy compression is required. Provider limits can still
reject a request explicitly. `task.submit` remains the validation/persistence
authority; Web creates no attachment filesystem, path authority or long-lived
copy. Owner-scoped task responses disclose attachment metadata only.
Activity adapters retain Main's authenticated owner transport and local/remote
Web session provenance. Task submission, safe retry, App-service admission and
protected owner App review remain backend responsibilities. The existing
capability-approval Web page is not a replacement for `cos review` or the native
OS review authority, and Activity text remains labelled untrusted owner data.
The Web is a presentation for separate `PackageImpact` reviews through that
same protected OS authority. App frames receive neither its token nor new
routes/privileges. Existing App-install/activation/update dialogs explicitly
reject package-impact reviews. Password handling remains exclusively in
`OsAuthorizationDialog` and the existing owner/token/origin/process/prompt-bound
authorization controller; positive HTTP clicks never call
`system.review.decide` directly.
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

The Workspace page and new-conversation picker use only `workspace.*` and
canonical conversation broker routes. Selection and drafts are presentation
state, not sharing or lifecycle authority. Mutations retain exact displayed
revisions, abortable reads reject stale generations, and failures never replay
writes. Workspace management remains available before model setup. See
[Workspaces](../../../../docs/workspaces.md).

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

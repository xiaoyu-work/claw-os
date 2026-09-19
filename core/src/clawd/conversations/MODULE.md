# Conversation Broker Module

## Purpose

Expose owner-scoped conversation operations over the existing durable session
and owner memory stores. These operations do not run an agent, submit a task,
or establish a frontend-owned history.

Per-turn model selection is validated through ordinary `task.submit`, not
conversation metadata. Creating or branching a conversation never loads a model.

## Key Files

| Path | Role |
| --- | --- |
| [`../conversations.rs`](../conversations.rs) | Owner checks, session issuance, presentation metadata and operations |
| [`dto.rs`](dto.rs) | Closed request/response types and identifier, title and limit validation |
| [`owner_memory.rs`](owner_memory.rs) | Owner-identity SQLite I/O without borrowing root filesystem access |
| [`jobs.rs`](jobs.rs) | Bounded projections of actual owner-scoped job records, not inferred execution outcomes |
| [`bindings.rs`](bindings.rs) | Owner/source checks, root-held inherited membership, and retained task-history proof |
| [`../../agent/memory/conversations.rs`](../../agent/memory/conversations.rs) | Bounded history snapshots, exact stored-row branches and replay exclusions |
| [`../../../test/unit/clawd/conversations.rs`](../../../test/unit/clawd/conversations.rs) | Broker regression tests |
| [`../../../test/unit/clawd/conversations/jobs.rs`](../../../test/unit/clawd/conversations/jobs.rs) | Owner/session job projection and metadata-bound tests |
| [`../../../test/unit/agent/memory/conversations.rs`](../../../test/unit/agent/memory/conversations.rs) | Persistence, branching and replay regression tests |

## Contract

- `agent.conversation.create` issues an empty, pending session through the same
  system-agent baseline as task submission. It creates no job. Root peers
  cannot adopt another owner, and conversation requests have no owner field.
- `get` and `list` include pending sessions that have no memory rows. Identity
  and creation time come from `SessionMeta`; a session ID is not permission.
- Responses also carry a stable UUID `presentation_id`. It is stored in
  root-owned presentation metadata for new sessions, with a deterministic
  read-only projection for older records. The existing whole-session-ID hash
  projection is preserved; no timestamp is inferred from the ID. Resolve a
  fork's canonical `parent_id` through the same owner-scoped `get` operation
  when its parent's presentation UUID is needed.
- `get.id` accepts either the canonical `ses_*` ID or its presentation UUID.
  UUID lookup searches owner-scoped canonical identity metadata, not a recent
  history/list window, so an old, archived or deleted conversation remains
  addressable after restart. The returned `id` is always the canonical ID.
  Ambiguous aliases are errors; root is not an ownership exception. Mutations
  continue to require canonical IDs, and clients cannot choose or change UUIDs.
- `state.json["conversation"]` contains presentation only: a manual title
  override, archive/delete flags, a parent display reference, a presentation
  UUID, and the time of a presentation edit. Existing generated memory titles
  remain the fallback. None of these fields supplies capabilities or a
  lifecycle authority.
- `update` persists rename, archive/unarchive and soft delete/restore. Soft
  delete hides an entry from lists, but its exact ID remains readable for
  restoration. Neither delete nor archive purges evidence or undoes effects.
- `list` returns unarchived conversations unless `archived: true` is supplied;
  deleted entries are omitted from either view. Results are newest-first.
  `conversation_count` counts the owner/archive-filtered visible entries before
  the requested limit; `conversations_truncated` identifies a limited view.
- `fork` creates a fresh session with freshly derived baseline capabilities.
  Its parent reference is not `SessionMeta::parent_session`, which is reserved
  for delegation. Existing Activity associations retain their owner and active
  Activity admission checks; source grants, budgets, credential tiers, runtime
  state and execution records are not copied.
- Task-backed history records explicit `JobExecution.id` membership at the
  actual outer user-message insertion and at each persisted message insertion.
  The recording identity is separate from cancellation scope. Responses expose
  `task_id`, `source_session_id`, `source_message_id`,
  `source_user_message_id` and `is_user_prompt` only after broker validation.
- `task_bindings_complete: true` means all active visible messages form complete
  recorded task memberships whose actual jobs match the owner and source
  session. `jobs` is then the retained task sequence, including ancestor tasks
  for forked prefixes; its `session_id` remains the original execution source.
  Fetch source journals by their real job ID through normal owner-scoped task
  routes, never by forging a child job.
- Fork lineage retains exact allowed source task/member identities in root-owned
  session presentation state. The broker checks owner-scoped ancestry, member
  coverage and real job ownership/session before an inherited lookup. User memory
  cannot select unrelated or foreign jobs.
- A fork retains all active stored history unless `before_user_turn` supplies
  the number of user prompts to retain. Zero retains no message rows. The
  explicit outer-user binding distinguishes prompts from tool-result user-role
  rows, including literal marker text supplied by a user.
  Message content, roles, ordering, timestamps and the frozen system-prompt
  reference are retained without reconstructing provider messages from display
  text. The current runtime's stored replay representation remains authoritative;
  this API does not invent provider state that was never persisted.
- `revert` removes the last positive `user_turns` count from active replay
  membership in one SQLite transaction. Original message IDs and bytes remain
  in ordinary memory queries and FTS; session turn/mutation journals are
  unchanged. New messages remain replayable. This is not filesystem rollback.
  A revision check prevents a changed membership from being edited using an
  earlier validated snapshot. Boundaries that split one task's retry attempts
  or other recorded members are refused before mutation.

Mutations and forks hold the task store's existing session-admission lock and
refuse pending, running or approval-waiting tasks. A new task cannot race
between the idle check and the edit. Presentation changes do not revoke or
create grants.

Owner memory I/O uses a disposable Linux thread with the peer's filesystem
UID/GID, no broker supplementary groups, and no process capabilities. Its
credential changes are thread-local and cannot affect the broker's other work.

## Bounds and Persistence

History defaults to 1000 rows (maximum 10,000), returning the newest complete-row
suffix within an 8 MiB serialized-message budget. System and injected context
rows are excluded before the row limit; existing evidence-marker sanitization
and tool presentation parsing are shared with other chat surfaces. Lists
default to 100 entries (maximum 1000).

Conversation responses include canonical memory row `id` values,
`message_count` and `messages_truncated`. The count and returned active-visible
rows use one SQLite read snapshot. The truncation flag covers both row and byte
limits; callers must not treat a suffix as complete history.

`jobs` projects actual owner/session-matching job IDs, statuses, prompts,
recorded timestamps and errors from the existing task store. It keeps the newest
1000 observed records within a separate 2 MiB budget, with `job_count` and
`jobs_truncated`, and returns them oldest-first. It omits execution response
bodies, contexts, owner homes and worker/process details.

When `task_bindings_complete` is false, `task_bindings_error` explains why
history is unavailable as a task replay. Any accompanying job records are
independent execution evidence, not a retained replay mask. Legacy unbound
rows, missing/pruned source evidence, invalid lineage and partial task
memberships are never retroactively guessed from text, timestamps, role markers,
cancellation scopes or matching counts. Nonempty task-history fork/revert
refuses these cases. Reverted jobs and original message/binding evidence remain
stored; they disappear only from the retained projection, not the audit trail.

Retries retain multiple explicit outer-user rows for the same real job ID.
The frontend counts those rows when selecting a whole-job boundary; it must
not invent a job for each retry. Require complete bindings and untruncated
message/job views before translating a job selection into a user-row cutoff.

Branch/revert reads are bounded to 20,000 rows and 16 MiB of stored message
content; an oversized operation fails rather than silently dropping history.
Fork prefixes stop before the next user prompt. Replay exclusions are additive
SQLite state, and read-only access remains compatible with databases that have
not yet received the migration.

Timestamps are persisted session, memory or edit timestamps, serialized as UTC
RFC 3339. Corrupt timestamps are errors, not invented recency values.

## Validation

From the repository root:

```bash
cargo test -p cos --lib conversations:: -- --test-threads=1

# Recording and execution plumbing, including mock-provider lifecycle parity.
cargo test -p cos --lib -- agent::runtime::loop_:: agent::service:: \
  --test-threads=1 --quiet
```

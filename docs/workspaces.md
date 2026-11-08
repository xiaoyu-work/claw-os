# Workspaces

A Workspace is a persistent context domain above Activities. It groups shared
background, owner notes, read-only file/folder tags and related goals. It is not
a filesystem working directory or a new Agent daemon.

## Owner controls

```bash
cos workspace create "Research" --description "Shared research background"
cos workspace list
cos workspace show WORKSPACE_UUID
cos workspace tag WORKSPACE_UUID /home/example/Documents/research --revision 1
cos workspace note WORKSPACE_UUID "Audience" --content "Explain terms for beginners." --revision 2
cos workspace attach WORKSPACE_UUID ACTIVITY_UUID --revision 3
cos workspace archive WORKSPACE_UUID --revision 4
cos workspace restore WORKSPACE_UUID --revision 5
cos workspace delete WORKSPACE_UUID --revision 6 --confirm
```

Replace identifiers and paths with values from the local system. Every mutation
other than creation requires the revision actually shown by `show`; a competing
change requires a refresh, not an automatic retry with a newer revision.
`update` accepts `--name`, `--description` and `--context`. `note --note NOTE_UUID`
edits an existing owner note; Agent reports retain their provenance and require
a separate correction note. `forget-note`, `untag` and `detach` remove exact
selected records.

## Files and folders

A Workspace tag explicitly shares a source read-only. A directory tag includes
its current and future descendants; a file tag names that exact path. Multiple
Workspaces may refer to the same source without copying it.

The broker verifies actual source type and owner-readable access. Protected
stores and unsafe links are not eligible. A tag never grants write, execution,
external delivery, another App's private storage or model-provider consent.
Current source state and live sharing must be rechecked before returning bytes;
an index or old reference is not proof of freshness or current permission.

The existing filesystem `task.workspace.resolve`, TUI `/directory`, and
`--workspace` task argument continue to select process cwd only. They do not
select this context domain. Use the separate semantic selector:

```bash
cos agent ask "Explain the shared sources" --space WORKSPACE_UUID
cos agent chat --space WORKSPACE_UUID
cos agent service submit "Prepare an outline" --workspace-id WORKSPACE_UUID
```

Canonical conversations and Root-created Jobs retain an optional Workspace ID.
An Activity or existing conversation supplies it when omitted. A conflicting
selection is refused before consent, session changes or queue publication.
Existing conversations with messages or retained Jobs cannot be moved; select
another Workspace by starting another conversation. Activity membership changes
require idle, checked-retired work. Moving automatic work puts it on a review
hold; old-domain handoffs are not imported.

## Deliberate context and retrieval

Every request receives a small source-labelled background/note/resource prelude,
not the transcripts of every conversation. Owner notes and Root-Job-bound Agent
reports keep different provenance. Both are data, not system policy or proof.
Additional reads use the guarded `cos_workspace`, `cos_recall` and semantic
recall tools through the existing private Root worker channel. The model has no
Workspace management or sharing controller.

Workspace retrieval filters sessions before full-text ranking, limits, source
windows, listings and counts. Unassigned legacy work cannot recall new private
Workspace conversations. Global learned notes, App memory and unscoped todo
memory are unavailable to Workspace work; automatic global curation is disabled.
Owner Memory inspection remains all-owner and preserves canonical evidence.

```bash
cos workspace context WORKSPACE_UUID
cos workspace read-note WORKSPACE_UUID NOTE_UUID
cos workspace index WORKSPACE_UUID
cos workspace index-source WORKSPACE_UUID TAG_UUID --revision 3
cos workspace search WORKSPACE_UUID "related research" --limit 10
cos workspace read WORKSPACE_UUID /home/example/Documents/note.md --max-chars 4096
```

For another page, pass its `next_offset` as `--offset` and its digest as
`--source-revision`. Changed content requires restarting the read.

The independent personal-file index uses the owner's default
`~/.local/state/claw-semantic/semantic.db` and the pinned local
Qwen3-Embedding-0.6B model. Broker reads do not accept a database selector;
custom indexer XDG stores are not substituted automatically. Registering a
tag's directory explicitly adds it to the existing owner indexer, including
supported files not tagged here. Registration acknowledges configuration, not
completed indexing. Workspace tags constrain retrieval, not global indexing.
V1 supports bounded UTF-8 text, not binary-format extraction.

File search filters live shared paths before reading/ranking index snapshots.
Returned snippets are rechecked against current pinned file bytes and digest.
Unsafe, revoked and stale sources do not become context. Index timestamps,
similarity scores and old successful calls are not current facts or permission.

## Temporary cross-Workspace reads

Only a human can select another same-owner Workspace's exact note IDs and exact
files under selected tag IDs, a current Job or canonical conversation, and an
expiry within 24 hours:

```bash
cos workspace read-link DESTINATION_UUID --revision 4 \
  --source-space SOURCE_UUID --conversation ses_001953abcdef0_123456789abc \
  --notes '["NOTE_UUID"]' --sources '[]' \
  --expires "$(date --utc --date='+1 hour' --iso-8601=seconds)"
cos workspace revoke DESTINATION_UUID LINK_UUID --revision 5
```

Human previews carry both the selected link and its exact saved target. For a
Job selection:

```bash
cos workspace read-note DESTINATION_UUID NOTE_UUID --link LINK_UUID --job JOB_UUID
cos workspace read DESTINATION_UUID /home/example/Documents/source.txt \
  --link LINK_UUID --job JOB_UUID --max-chars 20
# Keep both selectors when continuing the displayed source revision:
cos workspace read DESTINATION_UUID /home/example/Documents/source.txt \
  --link LINK_UUID --job JOB_UUID --offset NEXT_OFFSET \
  --source-revision DISPLAYED_DIGEST --max-chars 20
```

For a conversation selection, replace `--job JOB_UUID` with
`--conversation CANONICAL_SESSION_ID`. A Job-only link cannot be previewed by
selecting its conversation, another Job or a later Job. `--link` without one
target, a target without `--link`, and both targets together are rejected.
The broker loads the actual same-owner Root Job/conversation, checks the live
destination and replay constraints, and rechecks the original selection.

Web and native preview buttons use the persisted link target rather than an
unbound global preview. Ordinary `POST /api/workspaces/DESTINATION_UUID/read`
bodies remain unchanged. Linked previews use this closed envelope:

```json
{
  "request": {"command": "note", "note_id": "NOTE_UUID", "link_id": "LINK_UUID"},
  "target": {"kind": "job", "id": "JOB_UUID"}
}
```

For a file, use `command: "source"`, `path`, `link_id`, `offset`, `max_chars`
and, for continued pages, `revision` inside `request`. Keep the same `target`
for every page. The private Agent request has no human target selector:
Root continues to derive its actual Job and conversation from the lease.

A temporary directory selection is not a recursive grant: select its exact
files. No transcript sharing, transitive read, automatic persistent import,
write, execution or App permission is created. Imported owner notes retain
their source Workspace; Agent reports never become owner-confirmed notes.

Visibility changes close future model projection as well as new retrieval.
Conversations affected by revoked shared context require a new conversation;
the owner can still inspect their original evidence. Temporary-context
conversations cannot be recalled by other conversations, forked into a wider
read scope, or automatically curated into shared findings/handoffs. A one-Job
selection cannot be replayed by a later Job. Already-sent bytes, admitted model
calls and completed effects cannot be unseen or rolled back.

These are normal broker/Agent data constraints, not kernel isolation of arbitrary
same-account programs, unmanaged App internals or manual terminal edits.

## Human presentations

- **CLI:** `cos workspace` uses the shared owner broker and exact revisions.
- **Claw TUI:** `/workspace` is the only name for the model-independent center.
  Its picker, context/notes, tags, local search/readiness and Activity assignment
  use owned asynchronous requests. Enter starts another bound conversation.
  Protected approvals retain focus; closing a center does not replay or stop work.
- **Agent Web:** Workspaces navigation and the new-conversation picker use the
  same backend. Revisions are decimal strings, stale selections are ignored,
  and mutations are never automatically retried.
- **Native Agent:** the Fluent Workspace center, file/folder picker, sourced
  notes, source/search diagnostics and conversation controls use the shared
  DTOs through the authenticated bridge. UI/bridge presentation protocol v4
  must upgrade together.

Management needs no model setup. The model is needed only for Agent execution
or local semantic query vectors.

Plain chat uses `/workspace UUID|none` to open another bound conversation and
`/directory PATH|home` to select filesystem cwd. Unsupported slash commands
remain local errors and are never submitted to the model.

## Archive and deletion

Archiving keeps background and membership but suspends new/further model work.
It does not undo an already-admitted model call or effect.

Deletion removes Workspace-shared context and tags, stops member automatic work
and pauses active member Activities. It requests cancellation through the
existing task authority. A pending cancellation is shown as `cleanup_pending`,
not already-retired execution. Repeat the confirmed deletion at the new
displayed revision to finish pending cleanup.

Source files, Activity goals, conversations, receipts and execution evidence
are not deleted. A deleted Workspace cannot be restored or silently converted
into global shared memory. Its identity remains a tombstone for retained work.
To place a preserved goal elsewhere, stop its tasks and explicitly detach it
while paused.

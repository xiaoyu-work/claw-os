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

The existing filesystem `task.workspace.resolve` and `--workspace` task argument
continue to select process cwd only. They do not select this context domain.

## Archive and deletion

Archiving keeps background and membership. It does not cancel an in-flight Job.

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

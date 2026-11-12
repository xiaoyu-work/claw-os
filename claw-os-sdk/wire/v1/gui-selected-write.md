# Draft GUI selected file write and reserved atomic save

This unpublished 0.2.1/ABI-1 source contract extends the existing protected
selected-read coordinator. It is not a full filesystem, trusted picker UI,
publication or platform/App pin adoption.

| Binding | Calls |
| --- | --- |
| Rust | `gui::request_file_write(node)`, `gui::file_write_status(id, node)`, `gui::reserve_file_save(id, node)` |
| Python | `gui.request_file_write(node)`, `gui.file_write_status(id, node)`, `gui.reserve_file_save(id, node)` |
| Node | `gui.requestFileWrite(node)`, `gui.fileWriteStatus(id, node)`, `gui.reserveFileSave(id, node)` |
| Go | `RequestFileWrite(node)`, `FileWriteStatus(id, node)`, `ReserveFileSave(id, node)` |

The request/status bodies contain only a native metadata inode and, for
status/reservation, its Root-issued `sw-...` reference. They carry no owner,
window, pathname, approval flag, lifetime or executable. Thin facades use full
argv `__gui-file-write request|status|reserve --request-stdin` and generated
closed validators. Normal Host session/process/cgroup checks remain mandatory.

Home metadata consent is separate. This signed surface's bound `fs.write`
declaration is only an eligibility upper bound. An existing parent/startup
write capability never selects content. A new proposal creates an ordinary
protected exact-owner review naming one existing regular file, this window,
write access and possible Root-reserved replacement. Only Root can consume the
exact decision once and issue separate finite normal capability authority.
`pending`, review acceptance and the reference itself do not mean selected.
Read selection never grants write; write selection never grants owner-content
read. `O_RDWR` requires both independently approved selections.

Once selected, ordinary binary native pwrite/append/truncate/fsync uses the
FUSE path. Root obtains the preimage under owner filesystem credentials and
uses the existing canonical session inverse store and MAC-chained mutation
journal. Its durable start must precede effects; successful replies require
required file/directory syncs and committed completion. Unknown completion
fences and retires the instance, retains unresolved canonical evidence and
returns an error, never fictional write success. Fault injectors remain test-only.

`reserve_file_save` requires that exact live write selection. Its closed
`GuiFileSave` result gives one opaque ASCII `.cos-save-...` name and the retained
native parent inode. The App joins that name to the already known target
parent, creates/writes/fsyncs private scratch and renames it only onto the
selected target. No owner-side temporary pathname is exposed. Root verifies
the selected target and pinned parent, materializes under owner credentials,
preserves ordinary mode bits, syncs file and parent, and records the canonical
replacement. Other sibling creation/replacement, owner-file unlink/rmdir,
general rename, execution and watch authority are not granted. Discarding
this private scratch is allowed.

Root-published own revisions update only corresponding selected resources after
verified canonical completion, without a new read grant or longer lifetime.
Keep the returned selection ID and original proposed `node` for status and
future reservations: that consent anchor remains valid across verified own
replacement and cannot be changed into another target.
In-place changes retain normal same-object read behavior. Atomic save gives
new opens the replacement while retained old read FDs/mappings retain the old
object; a retained scratch FD reads its own authored bytes, not later target
content. Detached old write FDs refuse further effects. External modification,
replacement or identity conflict still fails closed, not silently rebound.
There is no claim of compare-and-swap against an uncooperative external writer.

Original decision/GUI/metadata deadlines and normal policy/provenance checks
remain authoritative; there is no arbitrary one-minute cutoff or automatic
renewal. Whole checked cgroup retirement governs cached/mapped bytes. Pending
and unused owner reviews retire with the window, and failed cleanup retains
custody. Legacy scalar filesystem RPC remains refused in the controlled GUI.

This existing-file API never creates absent owner files. The separate
[one-shot creation draft](gui-selected-create.md) covers an exact absent child
without overwrite. Directory CRUD, delete/general rename/watch and completed
native product adoption remain separate gates. The canonical inverse path is UTF-8: non-UTF8 write targets
refuse before effect without lossy conversion; metadata and selected reads
continue to use byte-safe identities. Owner-write-only files that cannot supply
an owner-readable inverse are likewise refused, not snapshotted as Root.

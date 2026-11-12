# Draft one-shot GUI file creation / Save As

This unpublished 0.2.1 / ABI-1 extension creates one **absent regular file**.
It is not directory write, overwrite, delete, watch, execution, a picker or a
publication/pin adoption. Existing-file write/save APIs retain their semantics.

| Binding | Calls |
| --- | --- |
| Rust/Python | `request_file_create(parent_node, name)`, `file_create_status(id, parent_node)`, `reserve_file_create(id, parent_node)` |
| Node | `requestFileCreate`, `fileCreateStatus`, `reserveFileCreate` |
| Go | `RequestFileCreate`, `FileCreateStatus`, `ReserveFileCreate` |

The only proposed location is a directory inode already materialized in this
window's authenticated metadata view and one literal UTF-8 leaf. Inode 1 is
valid when it is that declared directory. Root retains the actual parent FD,
owner-Home-relative identity and exact leaf; an App supplies no path, owner,
window, grant, approval flag or lifetime. A subroot never widens to Home.

Names are 1–255 UTF-8 bytes and exclude `.`, `..`, slash, backslash, NUL and
control characters. Spaces, quotes and other literal punctuation are not shell
syntax. Unpaired surrogates/invalid UTF-8 refuse, never convert lossily.
Protected targets and Root's `.cos-save-` / `.cos-native-` staging namespaces
remain refused. A non-UTF8 parent identity also refuses before inverse/effect.

Thin clients use complete argv:
`__gui-file-create request|status|reserve --request-stdin`.
Generated closed validators enforce each DTO. Older Roots safely refuse an
unknown command; clients never retry through scalar filesystem/path execution.

An eligible signed surface's own `fs.write` declaration is only a ceiling.
Metadata, inherited/startup rights and an App-chosen leaf create nothing.
Root requires a separate ordinary exact-owner protected operation review for
this absent child/window/parent identity, consumes it once and issues normal
finite `Issuer::Approval` / private `GuiResource` authority. Read is independent.

The `sc-...` reference is not a grant. `pending`, `selected`, review acceptance
and a returned slot are **not saved**. `GuiFileCreateSlot` returns one Root-owned
`.cos-save-...` private name under the same known parent, its exact `target_name`
and original remaining deadline. Repeated reservation returns the same slot,
not new rights or renewed quota. One selection has one finalization attempt.

The App creates/writes/fsyncs that private scratch with ordinary native POSIX
IO, renames it onto only the selected leaf and syncs its parent directory.
Root forces atomic `renameat2(RENAME_NOREPLACE)` even for ordinary rename:
an existing or concurrently appearing file/symlink yields `EEXIST`, never
overwrite or adoption. General sibling creation/moves and directory CRUD stay
closed. Private scratch contains App-authored bytes, not owner content.

Before effects Root records the real Missing preimage in the existing
canonical inverse store and durably starts a MAC-chained mutation bracket.
Owner-credential staging, exact expected-image verification, file and parent
syncs and committed canonical completion precede successful native replies and
`created` status. The new regular file uses owner-private non-executable mode.
Unknown outcomes fence and retire the instance while retaining unresolved
canonical evidence. Known no-publication failures do not become committed
creation. The target becomes atomically visible at rename; this is not a claim
of global compare-and-swap against an uncooperative parent-directory writer.

After creation, opening owner content still needs an independent read
selection. Further owner-file modification uses the existing-file write API;
the consumed creation slot cannot become an overwrite lease. A retained
private scratch handle may retain its authored bytes, never later foreign
target content.

Original decision, GUI and metadata ceilings remain fixed. Polling, retries
and reservation do not renew them or reset the shared bounded window ledger,
slot/node/scratch budgets or mutation-attempt limits. Fresh parent identity,
permissions, source path, provenance, policy and process/cgroup checks remain
mandatory. All pending/unused reviews retire with checked window death; failed
cleanup retains custody. No live/local fixture result or native product
acceptance follows from this source contract.

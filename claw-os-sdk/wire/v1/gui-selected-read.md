# Draft GUI selected file read (0.2.1 source, ABI 1)

This draft is not publication, an artifact/pin adoption or completed native
filesystem integration. Home browsing first requires its independent protected
owner/window metadata decision. An App path getter, parent `fs.read`, a proposed
inode, pending request or displayed approval is never selected content.

Each facade provides `request_file_read(node)` and
`file_read_status(selection_id, node)` (idiomatic casing per language), using
the normal installed CLI stdin transport. `node` is the virtual inode from
native metadata in this window's filtered Home filesystem: Rust `MetadataExt`,
Python `stat().st_ino`, Node `stat().ino`, Go `syscall.Stat_t.Ino`. File names do
not travel through this request. A guessed or foreign-view inode grants nothing.
Nodes are view-local and bounded; a directory is not a selectable file.

| Binding | Calls |
| --- | --- |
| Rust | `gui::request_file_read(node)`, `gui::file_read_status(id, node)` |
| Python | `gui.request_file_read(node)`, `gui.file_read_status(id, node)` |
| Node | `gui.requestFileRead(node)`, `gui.fileReadStatus(id, node)` |
| Go | `RequestFileRead(node)`, `FileReadStatus(id, node)` |

Root derives the owner, registered signed surface, window, process/start,
session/cgroup and source identity. Root verifies the source through retained
descriptors and refuses directories, protected nodes, aliases, symlinks,
hardlinks and undeclared read authority. Presentation uses inert escaped bytes;
non-UTF8 names are never lossily converted into identity or authorization.

The existing protected owner review names the exact file, read access and
window. Its operation digest binds that proposal and retained source identity.
Only Root consumes the exact decision once, rechecks the source and fresh GUI
authority, and installs the bounded native read lease. A missing optional
capability is authorized through that ordinary exact owner decision and Root
issuance, not by mutating parent/global grants or the immutable startup GUI cap
set. No write/watch/general mutation authority is acquired.

The declared capability is an eligibility upper bound. In particular, a
non-UTF8 file can have an ordinary Home/subtree `fs.read` technical scope without
inventing a lossy exact-path scope: Root's private operation-bound permit and
retained node identity expose only that one file. No grant handle or directory
read right reaches the App. Fixed/owner-relative literal file/subtree needs are
supported; Wild needs require existing bounded Path inheritance and unbound
argument needs remain refused.

The response follows `gui_file_read.schema.json`. Only `state: "selected"`
means Root installed content access; `pending`, an approval ID or request
success does not. The `sr-...` identifier selects a status record, not a grant.
Status must name the same node and this same authenticated window. Clients
neither approve nor infer selection and contain no provider/authority logic.

Read consent has its own finite deadline and revocation generation and cannot
extend metadata or GUI authority. Its fixed deadline honors the approved
duration, clipped by the original metadata and GUI authority and any shorter
ordinary read grant; status polling cannot renew it.
Expiry/revocation, source replacement, owner
denial or package withdrawal denies further brokered reads and retires the
checked process tree, including separate process groups. Cached/mapped bytes do
not re-enter FUSE: checked instance retirement, not a fictional cache permission
check, is the revocation boundary. Already-read bytes cannot be erased.
Source revision checks are conservative: content, timestamp or link changes
invalidate the selection rather than silently following a changed object.
Window retirement invalidates pending reviews and retires unused approvals;
an owner approval remains historical evidence, not a fictional owner denial
or selected resource after that window has gone.

The existing entry ABI, signed entry admission, common Host, private App HOME,
protected paths and checked nonlazy unmount/provider retirement remain
mandatory. This adds no App-provided owner/window/path authorization, alternate
approval command, live model dependency or raw Home fallback.
Legacy scalar `cos_runtime::filesystem` / `__filesystem` read/write/replace is
not a selection alternative in this controlled GUI. Use native POSIX/FUSE IO
only after Root reports the exact file selected; private App state keeps its
existing partition and does not become owner-Home authority.

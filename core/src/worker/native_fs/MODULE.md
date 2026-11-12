# Filtered Native Home Resources

The Linux production provider defaults to a read-only Home metadata view in
the common authenticated GUI Host. Its bounded content operations are separately
owner-approved regular-file reads/writes and a Root-reserved replacement of an
already selected existing writable file. Watch, direct prototype selectors and
mutation fault injectors remain `cfg(test)`; directory creation/delete/general
rename are not implemented. This is not a public App mount API, frozen SDK contract or
completed `controlled_full` filesystem. A Root controller supplies explicit
per-window metadata consent, an owner-accessible pinned browse root and a
bounded lease. Only that controller can bind an actual normal-UID process and
its checked cgroup or choose resources. No FUSE request prompts or grants.

The private API is `NativeFsView::create(owner, browse_root, mountpoint,
lifetime)`, `create_writable(..., root_session)`, one-use
`bind(process_identity, cgroup_fd)`, independent `choose_read(relative)`,
`choose_write(relative)` and `choose_watch(relative)`, Root-only
`reserve_save(selected_target)`, `health()`, `revoke()` and
`retire_checked(deadline)`. Read choices pin one file or directory; directory
reads walk only beneath the retained descriptor. Writes currently select
existing regular files, never a directory, delete right or executable.
All choices expire with the nonrenewable view lease (at most five minutes).
These content APIs are private test-build APIs, not a frozen contract.

The production entry is `create_home_metadata`: its `MetadataRoot` source
retains the authenticated NSS owner Home plus one exact, no-follow directory
descriptor beneath it, matching both inode identities in the protected window
decision. A full-Home view uses an empty relative prefix. A subroot view's inode 1
is that declared directory, while every node and selected-file candidate keeps
its authenticated Home-relative name (for example `Projects/review/leaf`).
Only descriptor IO strips that prefix; it never becomes the authorization path.
Open/walk and protected-object checks independently refuse outside that root.
Its process/cgroup binding is one-use. `renew_home_metadata`
is controller-only, requires a live bound instance, lasts at most two seconds
and cannot cross the original consent's monotonic ceiling. The GUI controller
performs fresh grant, package, parent, owner policy and consent-generation
checks before each renewal. The test-only `choose_read` API still refuses
metadata views: their content admission requires the real Root decision and
ordinary authority proof described below, even in a test build.

## Protected selected file reads

The four draft SDK GUI facades propose only a bounded virtual FUSE inode and
query an opaque status ID. They never supply owner, window, source path or a
grant. Root authenticates the exact live GUI peer/cgroup, retains the materialized
node's no-follow descriptor, raw relative pathname, device/inode and revision,
then checks that this selected signed surface declares eligible `fs.read`.
Literal fixed/owner-relative file or subtree scopes are supported. A Wild
declaration can use only already-bound, bounded Path caps; absent Wild bindings
and argument-bound requests do not invent a Home grant.

Every new selection files a separate ordinary protected owner review naming
the exact escaped target, read-only access and window. The operation digest
binds the raw pathname bytes, retained identity/revision, node, surface, display
and live GUI authority. Pending, an approval ID and a proposal's display text
are not content. Another owner, node, window, changed source or consumed/stale
decision cannot install a read lease. The current source fingerprint is strict:
external content/mtime/ctime/link changes invalidate consent rather than
following an external writer or replacement. Only verified own canonical effects
may advance the corresponding selected resource revision.

Root consumes the exact decision once and issues a separate normal
`Issuer::Approval`, process-tree-bound `GuiResource` grant in the existing store.
It preserves App/session, Activity and Workspace checks and never changes the
startup GUI cap set, parent/global grants or the session index. For a non-UTF8
target the declared UTF-8 capability scope remains the technical upper bound;
the unexported Root-held grant and operation-bound native lease expose only the
one pinned file, not its siblings or directory. The status ID is not a grant
handle. Names stay Unix bytes; only inert presentation is escaped.

The fixed finite read deadline cannot exceed its own approval, live GUI grant
or original metadata consent, including a shorter ordinary read grant. There
is no automatic read renewal. Every native open/read rechecks source identity and ordinary
authority, owner denial, declaration/package ceiling, GUI provenance and both
decision generations. The independent provider custody thread also detects
read expiry. Revocation/expiry/source failure fences the view and retires the
whole checked instance, including separately grouped descendants, because
held FDs and mmap/page-cache bytes cannot be revoked by a fictional per-read
check. Already-read bytes cannot be erased.
After checked process death, pending private read reviews are invalidated by
the system and unused approved decisions are archived without rewriting the
owner's decision. Store failures retain proposal custody for cleanup retry.

Home-scoped filesystem capabilities remain authority, not raw bind requests,
when this explicit metadata surface uses the controlled view. Optional startup
resource validation applies the same mount projection without treating it as
window consent. Actual mounting still waits for the separate owner decision.
Read/write/delete/watch/exec scopes beneath Home are suppressed only from raw
mount derivation; the FUSE provider retains its protected-path/alias checks,
noexec filtered mount and closed general-mutation/watch APIs. Scalar
filesystem read/write/replace relay is refused for this controlled GUI so it
cannot bypass selection. Ordinary/MCP and non-metadata GUI behavior is unchanged.
Mandatory Home delete/watch/exec needs remain refused while their native
resource integration is unavailable; write authority still needs its separate
selected-file decision before any effect.

`statfs` is metadata: after the same live peer and protected-node checks, it
uses owner filesystem credentials and `fstatvfs` on the retained browse-root
descriptor. Counts describe the backing filesystem; width overflow fails rather
than reporting fabricated capacity. No global path lookup or file-content
authority is introduced, and the App's filesystem magic remains FUSE.

Internal limits are 32 selections of each kind, eight exact save reservations,
4,096 nodes, 128 handles, 64 path components, 4,096 pathname bytes, 128 KiB per
backend read/write and 8 MiB per changed file. A view charges at most 64 MiB
of cumulative private-scratch growth and 64 MiB of inverse snapshots. Failed
or discarded operations do not refund these conservative lifetime budgets.
At most 256 owner mutation attempts may prepare canonical inverse records;
failed starts cannot grow an unbounded snapshot/inode log.

`metadata_scope.rs` pins and revalidates the authenticated Home/directory pair.
Every authority-bearing native operation, including retained directory/file or
private-scratch handles, passes the shared live gate. That gate freshly walks
authenticated Home to the literal root under owner credentials and checks owner
access, no-follow paths, protection and retained endpoint identities. Failure
latches across all source clones and revokes the view; restoring paths or modes
cannot revive the window. Health, renewal and independent custody monitoring use
the same gate, including when no App IO reaches FUSE. Cleanup-only releases,
stop and checked retirement do not depend on source validity. This fences and
retires held descriptors/mappings; it cannot erase already-read cached bytes or
validate individual memory loads.
`mod.rs` owns the private FUSE connection/mount, monotonic expiry, worker health
and checked process/mount/thread retirement. `backend.rs` separates native
lookup/stat/readdir from independently selected binary reads and writes.
`selected_read.rs` is the Root-only pinned candidate/read-permit seam; its
provider handle is weak so status custody cannot prolong a retired backing
view. Proposals and read pins close only after checked instance death; the
existing exact nonlazy unmount/provider join precedes group/runtime removal.
Backing IO uses authenticated
owner filesystem credentials, pinned directory walks, actual object identity
and the existing protected-path/alias guard. Names remain Unix bytes.

The test default constructor mounts a read-only filesystem. The controlled
Home proxy is write-capable at the mount layer so a later exact write selection
can work; metadata alone still supplies no writable file or creatable name.
The test writable constructor only enables the internal data plane: it grants no file rights.
Both refuse symlinks, hardlinked regular files and special nodes. No raw browse-root
bind or backing FD reaches the App.
The App sees only the filtered FUSE mount through the ordinary namespace
sandbox; `/dev/fuse` and provider control are absent.
The view's Root-owned parent permits traversal for the bound normal UID;
bubblewrap resolves even an inherited `O_PATH` source through its parent.
This does not expose the backing tree or allow another process through the
view's instance/cgroup authorization.

There is deliberately no passthrough or keep-cache grant. Ordinary FUSE page
cache supports selected read-only mmap, and cached/mapped bytes do not
re-enter the backend. A selected file remains authorized only for this one
instance, and expiry, revocation or worker failure stops new backend operations
and triggers checked cgroup retirement. Completion is not acknowledged until
the process tree is empty, the exact mount is unmounted without lazy detach,
and all three provider threads have joined. Already-read bytes are not erased.
The custody monitor checks every 10 ms and retries failed process retirement;
this is not a promise of instantaneous cache or memory erasure at the deadline.
Failed checked cleanup retains the view and refuses replacement.

## Selected writes and atomic save

The production coordinator reuses read's exact-owner/window review, fixed
deadline, ordinary `Issuer::Approval` issuance and complete review retirement.
Its `WritePermit` additionally requires an unforgeable spent `FS_WRITE` proof,
Root controller, owner-accessible pinned target and Root-owned canonical
mutation session. Missing optional write authority is acquired only through
that explicit protected decision. No startup or parent cap selects content.
The canonical evidence session starts with no capabilities and is not authority.
Both native read and write selections keep their original approval/GUI/metadata
ceilings; neither metadata refresh nor own saves extends a deadline.

`choose_write` checks its retained Root controller, live lease, owner permissions
and exact pinned file identity. It never grants content read to the App.
`O_RDWR` still requires a separate read selection. The canonical inverse
recorder needs owner-readable previous bytes; a file the owner can write but
cannot read is refused rather than snapshotted under Root's filesystem identity.
Non-UTF8 write targets are refused before selection because the existing
canonical inverse path is a UTF-8 string. Read/metadata names remain Unix bytes.

Native pwrite/append/truncate/fsync use the selected file and owner credentials.
Writable FUSE handles use direct IO; writeback and writable mmap cannot bypass
the mutation brackets. The mount remains `nosuid,nodev,noexec`, and resource
execute bits are never exposed. `fs.write` does not become `fs.delete` or
`fs.exec`: unlink/rmdir of owner content, general moves/renames, new owner-file
creation, arbitrary writable siblings and writable directory selection remain
unsupported.

For atomic save Root reserves one opaque, same-parent temporary name for one
already selected target. Only that exact name can be created as private bounded
scratch in this view. The App cannot allocate reservations through FUSE, choose
a different replacement target, persist the scratch as another sibling, or
unlink any existing owner file. Discarding that private scratch is allowed.
Its bytes never appear at the owner-side temporary pathname.

Rename materializes the private bytes through a pinned parent under owner
credentials, checks the selected target and parent identity, syncs the new file,
renames only onto the reserved target and syncs its directory. File mode is
preserved without setuid/setgid; ownership is the authenticated writer's, never
manufactured from a different owner. Old read FDs retain the provider-replaced
object; a retained scratch FD reads its own authored snapshot, not subsequently
unselected external content. Existing independent read selections follow the
provider's checked replacement; no new read selection is minted.

Production file candidates retain an immutable consent anchor and a private
current revision. Only a committed canonical effect with verified resulting
descriptor/fingerprint can publish that next revision to matching selected
read/write leases. In-place writes retain same-object FD semantics. Atomic save
uses a new FUSE inode: new opens see the replacement while old read FDs/mappings
retain the previous object. Historical pins are bounded, checked for external
changes and released after the last corresponding native handle closes.
Detached old write FDs refuse effects; a retained scratch FD reads its own
authored bytes, never later target content. Pending proposals do not inherit
these revision changes.

The existing effect mutex serializes provider effects and revision publication.
Health/metadata heartbeats defer only the source snapshot while that controlled
operation owns the mutex; live GUI/ordinary grant/expiry checks remain. Cache
invalidations are queued to the existing observer, never sent while holding
the effect/dispatcher locks. Observer snapshots cannot overwrite a newer own
revision. External conflicts still fence rather than follow or grant.

As with the existing bounded `clawd/file_changes.rs` provider, the final
identity check followed by rename is not a filesystem compare-and-swap against
an uncooperative host writer. No wider external-writer serialization or full
POSIX metadata/xattr contract is claimed.

## Canonical evidence and failure

Writable construction retains a Root-authored session belonging to this owner.
Every owner-file effect uses the existing `session::record_fs_write_bytes`
inverse store and `session::journal::MutationBracket`, with the existing
`system.filesystem.write` or `system.file.replace` operation label and the
inverse mutation sequence. These are evidence references, not grants.
Production starts also name the keyed ordinary selected-write grant reference.
There is no second journal or App-supplied session/owner override.

The start must durably land before dispatch. File and required directory syncs
must finish before a committed closure and successful FUSE write/rename reply.
An uncertain IO result or failed completion record leaves the canonical bracket
unresolved, returns EIO, fences the view and retires its process tree. A new
writable view refuses a session with unresolved effects. Cleanup never resolves
the effect, deletes the published target or treats a vanished pathname as proof
of retirement. Pending internal backing stages retain directory/file identities;
checked unlink proves the retained file has no links and syncs its parent.

## Backing changes and native watches

A separately joined observer compares owner-accessible metadata fingerprints
for already materialized nodes every 25 ms. It invalidates kernel data/attribute
caches without holding the dispatcher locks. Zero entry TTL and fresh directory
enumeration expose changed metadata without granting new content access. External
replacement does not silently transfer an old object selection.

An explicitly Root-selected regular-file watch supports native
`poll(fd, POLLPRI)` and `FUSE_NOTIFY_POLL`; readiness follows cache invalidation.
`FUSE_NOTIFY_DELETE` marks the exact cached object unlinked. File-watch
`IN_DELETE_SELF` follows the last dentry reference closing; a retained open FD
delays it. This does not inject a parent-directory `IN_DELETE` event.
Parent-change handling must not pre-evict the child dentry needed by the exact
delete notification.
App-origin writes/atomic rename produce normal kernel inotify events.
FUSE 7.31 cannot inject general external create/modify inotify events:
cache invalidation is not such an event, and no fake write, grant or JSON
metadata substitute is used to pretend otherwise.

The existing private GUI fixture's native-FS increment uses a synthetic owner
tree and a normal-UID native probe. Its Root fixture stands in for the future
OS selection controller, not an App-selected path or authority-bearing CLI.
The probe enters through the common App runner, which consumes its launch gate
and closes inherited setup descriptors before executing native code.
Unavailable FUSE, namespaces or cgroup facilities fail this explicit fixture.

## Common GUI Home metadata

The signed selected surface must have an explicit, unconditional mandatory
`fs.meta` recursive directory need, using `owner-path: "**"`, one literal
owner-relative `subdirectory/**`, or the exact canonical absolute equivalent.
Bare directories, missing/non-directory roots, aliases, protected roots and
multiple distinct metadata views refuse; there is no raw Home-child fallback.
Legacy operation unions and broad inherited
parent rights are not window consent. The Host files an ordinary protected
exact-owner approval under this unique GUI session and an operation digest of
the registered window, display epoch, surface, launcher and Home identity.
Restoration receipts, another owner's decision, another window or a different
surface cannot satisfy it. Optional metadata does not create a startup prompt.

The approved Home metadata cap is removed only from raw-bind derivation, never
from the real GUI authority. The FUSE view replaces Home inside the namespace;
`HOME`, App data and XDG fallback state remain the private App partition.
An independent Home content mount is still refused. `COS_OWNER_HOME` remains
context, not a grant. File selection uses existing protected OS review, not
an App-drawn picker, App-name exception, raw Home bind or alternate approval
endpoint.

The existing protected setup runner waits before bubblewrap. Root binds its
actual PID/start/pidfd and exact checked instance cgroup, validates the live GUI
grant and installs the metadata lease before releasing sandbox setup. The
existing inner App gate is held until transport and creator checks finish.
Metadata admission is not UID-only and never authorizes all `clawd` descendants.

Cleanup stops metadata admission, checks the process tree dead, then closes
Root bind-source pins, unmounts without lazy detach and joins the provider.
Only then can the cgroup and runtime be removed. Failed preparation/retirement
retains custody rather than treating an absent process, manager or pathname as
proof. The existing GUI/PAM fixture adds one signed metadata-only surface with
inherited parent metadata rights but a separate window decision, protected-name
filtering, denied content, private state and checked revoke lifetime. Its inner
App proves real FUSE with `fstatfs`; Root also checks the exact retained FUSE
mount. Capacity fields are compared with the owner-accessed backing filesystem.
The single selected-read case exercises both missing optional authority and
already-inherited broad Home filesystem rights. It uses the actual Rust SDK,
normal protected terminal review/Root decision, non-UTF8 binary IO/seek/mmap,
unselected siblings and checked independent read revocation. None of those
capability descriptions selects content before the exact file decision.

The generic subroot GUI fixture additionally requires real FUSE at inode 1,
independent panel admission, protected-name filtering, no parent/sibling metadata,
unknown-node refusal, an exact owner-window metadata decision and a separate
selected read whose Home-relative target differs from a same-named Home file.
It checks the source filesystem's capacity and checked whole-instance retirement.
These added source regressions still require actual Linux execution.

Actual Linux acceptance of production selected reads/writes/reserved saves,
watch integration, complete
directory/delete/rename semantics, non-UTF8 inverse paths, writable mmap,
external-writer concurrency, complete external inotify behavior, real OS
picker UI, persistent grants, SDK publication and real Files/Editor
adapters remain publication gates. No fixed-folder or read-only release is implied.

## Private Linux kernel check

Build on Linux with the existing toolchain (Rust 1.90+ for the existing native
probe); do not build native products on Windows/NTFS:

```sh
bash scripts/ci/test-boundaries.sh native-fs

# The underlying compiler commands:
cargo test --locked -p cos --bin cos --no-run
cargo build --locked --manifest-path desktop/comp/test/display-control/Cargo.toml --bin claw-gui-probe
```

Use compiler-selected Root test and probe ELFs, not a guessed binary filename
or production SDK payload. Run the existing fourteen-input private bootstrap
with its additional `--native-fs-only` selector:

```sh
sudo unshare --mount --net --fork bash "$BOOTSTRAP_SCRIPT" \
  "$ROOT_TEST_ELF" "$DISPLAY_HOST_ELF" "$COMPOSITOR_ELF" "$PAM_MODULE_SO" \
  "$PAM_FIXTURE_ELF" "$PRIVATE_LINUX_DIRECTORY" "$COS_ELF" "$GUI_RUNNER_ELF" \
  "$APP_RUNNER_ELF" "$PROBE_ELF" "$DISPLAY_SESSION_ELF" "$COSMIC_SESSION_ELF" \
  "$FILE_MANAGER_ELF" "$FILE_MANAGER_AUTOSTART" --native-fs-only
```

`BOOTSTRAP_SCRIPT` is `crates/claw-display-login/test/fixtures/bootstrap.sh`.
The private directory must be a fresh, project-owned directory on a Linux
filesystem. Prerequisites are real `/dev/fuse`, Root mount/network namespace
privileges, writable delegated cgroup v2 with cpu/memory/pids controllers,
bubblewrap 0.8+ with unprivileged user namespaces/seccomp, and the existing
bootstrap's PAM/NSS/overlay utilities and matching compiler-selected helpers.
No libfuse development package or fusermount privilege is needed by this
manual Root mount with `fuser = 0.16.0`, default features disabled.

This selector runs only `clawd::server::gui_fixture::private_native_fs_kernel`;
it does not alter baseline GUI/PAM cases. One existing test exercises metadata
without content, exact file and directory selection, binary/non-UTF8 names,
native mmap-before-read and seek, forbidden/alias/owner-permission refusal,
readonly behavior, retained FD/mapping plus a `setsid` descendant, monotonic
expiry and actual in-process FUSE worker failure. Its write branch exercises
independent write/read refusal, native binary write/seek/truncate/fsync,
reserved atomic save and retained old/new FDs, no sibling/delete/exec authority,
canonical inverse and MAC-chain correlation, real poll/inotify/cache updates,
refusal before an unrecordable start and unresolved post-effect completion with
checked process retirement. Root fixture choices are
not the final authenticated selection UI. Root-daemon crash/restart recovery
and production GUI/selection integration are not established by this test.

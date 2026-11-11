# Private Native Filesystem Kernel Prototype

This Linux-only, test-build internal provider is not a public App mount API, SDK contract
or completed `controlled_full` filesystem. A Root controller supplies explicit
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
This is not a frozen API.

Internal limits are 32 selections of each kind, eight exact save reservations,
4,096 nodes, 128 handles, 64 path components, 4,096 pathname bytes, 128 KiB per
backend read/write and 8 MiB per changed file. A view charges at most 64 MiB
of cumulative private-scratch growth and 64 MiB of inverse snapshots. Failed
or discarded operations do not refund these conservative lifetime budgets.
At most 256 owner mutation attempts may prepare canonical inverse records;
failed starts cannot grow an unbounded snapshot/inode log.

`mod.rs` owns the private FUSE connection/mount, monotonic expiry, worker health
and checked process/mount/thread retirement. `backend.rs` separates native
lookup/stat/readdir from independently selected binary reads and writes.
Backing IO uses authenticated
owner filesystem credentials, pinned directory walks, actual object identity
and the existing protected-path/alias guard. Names remain Unix bytes.

The default constructor still mounts a read-only filesystem. The writable
constructor only enables the internal data plane: it grants no file rights.
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

Write/atomic-save/backing-watch kernel acceptance for this increment, complete
directory/delete/rename semantics, non-UTF8 inverse paths, writable mmap,
external-writer concurrency, complete external inotify behavior, real OS
selection UI, persistent grants, public four-language APIs and real Files/Editor
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

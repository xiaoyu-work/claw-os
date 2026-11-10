# Private Native Filesystem Kernel Prototype

This Linux-only, test-build internal provider is not a public App mount API, SDK contract
or completed `controlled_full` filesystem. A Root controller supplies explicit
per-window metadata consent, an owner-accessible pinned browse root and a
bounded lease. Only that controller can bind an actual normal-UID process and
its checked cgroup or choose read resources. No FUSE request prompts or grants.

The private API is `NativeFsView::create(owner, browse_root, mountpoint,
lifetime)`, one-use `bind(process_identity, cgroup_fd)`, `choose_read(relative)`,
`health()`, `revoke()` and `retire_checked(deadline)`. File choices pin one
object; directory choices walk only beneath the retained directory descriptor.
All choices expire with the nonrenewable view lease (at most five minutes).
Internal limits are 32 choices, 4,096 nodes, 128 handles, 64 path components,
4,096 pathname bytes and 128 KiB per backend read. This is not a frozen API.

`mod.rs` owns the private FUSE connection/mount, monotonic expiry, worker health
and checked process/mount/thread retirement. `backend.rs` separates native
lookup/stat/readdir from selected binary reads. Backing IO uses authenticated
owner filesystem credentials, pinned directory walks, actual object identity
and the existing protected-path/alias guard. Names remain Unix bytes.

The prototype is read-only and does not implement backing-change watches. It
refuses symlinks, hardlinked regular files and special nodes. No raw browse-root
bind or backing FD reaches the App.
The App sees only the filtered FUSE mount through the ordinary namespace
sandbox; `/dev/fuse` and provider control are absent.
The view's Root-owned parent permits traversal for the bound normal UID;
bubblewrap resolves even an inherited `O_PATH` source through its parent.
This does not expose the backing tree or allow another process through the
view's instance/cgroup authorization.

There is deliberately no passthrough or keep-cache grant. Ordinary FUSE page
cache is needed for this first mmap probe, and cached/mapped bytes do not
re-enter the backend. A selected file remains authorized only for this one
instance, and expiry, revocation or worker failure stops new backend operations
and triggers checked cgroup retirement. Completion is not acknowledged until
the process tree is empty, the exact mount is unmounted without lazy detach,
and both provider threads have joined. Already-read bytes are not erased.
The custody monitor checks every 10 ms and retries failed process retirement;
this is not a promise of instantaneous cache or memory erasure at the deadline.
Failed checked cleanup retains the view and refuses replacement.

The existing private GUI fixture's native-FS increment uses a synthetic owner
tree and a normal-UID native probe. Its Root fixture stands in for the future
OS selection controller, not an App-selected path or authority-bearing CLI.
The probe enters through the common App runner, which consumes its launch gate
and closes inherited setup descriptors before executing native code.
Unavailable FUSE, namespaces or cgroup facilities fail this explicit fixture.

Write/atomic-save/journal/watch semantics, real OS selection UI, persistent
grants, public four-language APIs and real Files/Editor adapters remain
publication gates. No fixed-folder or read-only release is implied.

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
expiry and actual in-process FUSE worker failure. Root fixture choices are
not the final authenticated selection UI. Root-daemon crash/restart recovery
and production GUI/selection integration are not established by this test.

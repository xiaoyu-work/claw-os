# Compositor Smithay dependency

`smithay/` is the complete 333-file upstream snapshot from
[`Smithay/smithay`](https://github.com/Smithay/smithay) commit
`211c19d712dc5f1b78eb879f1c195715801e37ff`, the compositor's previous Git pin.
Its [MIT license](smithay/LICENSE.txt), build inputs, examples, tests and
five upstream workspace members are retained. There are no Git submodules or
other projects vendored here. The upstream workspace manifest is unchanged
and excluded from the compositor workspace.

Upstream trailing whitespace is preserved for byte-identical provenance.
Repository whitespace checks remain enabled for the three locally modified
files; only trailing-space checks on the imported snapshot are exempted.

The local production patch changes only these upstream files:

- `src/wayland/selection/mod.rs`: compatibility-default
  `SelectionHandler::allow_selection_read(client, seat, target)`.
- `src/wayland/selection/seat_data.rs`: check the recipient before inspecting
  selection state, constructing an offer or disclosing MIME types. Denial
  preserves public initial/focus unavailability, not private change signals.
- `src/wayland/selection/offer.rs`: retain the server-selected target and
  recheck the requester before either client- or compositor-provided payload
  transfer, including an already-created offer.

The default returns `true`. **This foundation does not enable OS clipboard
permission enforcement.** The compositor does not override it yet. It grants
no authority, changes no writes or DnD behavior, and cannot revoke transfer FDs
already delivered to another process. Root/Host instance binding, effective
permission checks, separate layer-shell admission and revocation remain a
subsequent coordinated unit.

The [private dispatch fixture](../test/selection-read/Cargo.toml) is a separate
test-only workspace. It consumes this same source with only the Wayland
frontend and the compositor's pinned Wayland server/backend versions, rather
than building the upstream reference compositors. Its client protocols are
also pinned. It uses socket pairs and synthetic data, not a desktop socket,
session bus, user selection or graphics backend. Coverage includes initial
and updated offers from both provider kinds, held-offer permission changes,
target isolation, default compatibility and an actual DnD payload transfer.
It also records nullable selection events: empty and populated initial state
must look identical to a denied client, and later set/clear transitions from
either provider kind must produce no notifications for that client.

The pinned `SeatData::send_selection` callers distinguish notification causes
without keeping a mutable permission history:

| Existing caller | `restrict_to` | `update_data_control` | Denied recipient |
| --- | --- | --- | --- |
| WLR/EXT device creation | New device | `true` | Initial null event for each supported target |
| Core/primary focus change | `None` | `false` | Null focus notification |
| Set, clear or source destruction | `None` | `true` | No selection notification |

The WLR/EXT protocols require an initial selection event on device binding.
Core/primary notifications follow their existing focus path. These public
lifecycle events report unavailable content regardless of private state;
selection mutations cannot become observable through repeated null events.
Default-allow clients retain the original initial, focus and update behavior.

From the OS repository root on Linux/WSL:

```bash
CARGO_TARGET_DIR="$PWD/build/smithay-read-hook-target" \
  cargo test --manifest-path desktop/comp/test/selection-read/Cargo.toml --locked
CARGO_TARGET_DIR="$PWD/build/smithay-compositor-target" \
  cargo build --manifest-path desktop/comp/Cargo.toml --locked
```

The compatibility build needs the compositor's ordinary native development
libraries, including `libpixman-1-dev`; those dependencies are not vendored.

Keep dependency updates explicit: replace this snapshot only with a verified
real upstream revision, preserve its workspace/license, reapply and review the
three-file patch, update the compositor lock, and repeat both commands.
Never edit Cargo's shared Git cache or substitute a fabricated revision.

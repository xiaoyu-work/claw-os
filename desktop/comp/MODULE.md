# OS Compositor

This GPL COSMIC fork keeps its original [license](LICENSE) and
[component provenance](../PROVENANCE.md). It owns Wayland protocol dispatch,
not App permissions, login authentication or provider implementations.

The compositor starts only through the
[Root display activation](../../core/src/display_session/MODULE.md). Its
private control descriptor and authenticated parent binding are required before
protocol dispatch. The former owner-created `COSMIC_SESSION_SOCK` exchange is
removed; `session.rs` retains only the in-process environment projection.

`src/display_authority.rs` consumes Root creator leases and records accepted
clients and listeners. `context_created` consumes an installed creator once;
client-supplied engine/App/instance strings cannot bind authority.
Ordinary login clients have a separate kernel-login origin, never App selection
rights. Layer-shell admission is separate from selection and cannot come from
a Panel label. The Root GUI Host projects only the live grant's explicit
`ui.panel:Wild` into that lease field. It authorizes the App's own overlay
surface/focus, not OS-session-only virtual keyboard/input method, capture,
desktop-management or other global protocols.

`src/wayland/handlers/selection.rs` applies independently granted read/write
rights to the [pinned Smithay hooks](vendor/README.md). Unbound, retired or
expired instances fail before offers, MIME disclosure, payload forwarding or
selection mutation. All four selection protocols are covered; DnD is separate.
Retirement removes listeners and all accepted clients, while Root confirms
the actual instance and descendant teardown.

The private headless workspace under `test/display-control/` embeds these
production authority/security-context handlers. Its synthetic native client
is run by the actual Root Host and full worker sandbox. Its compositor-side
write witness verifies real write-only data and clears without giving the
writer read authority. No real desktop, clipboard/history or renderer backend
is used. Existing dispatch/privacy/default-allow regressions remain under
`test/selection-read/`.
The authenticated process fixture creates real overlay surfaces with explicit
panel grants, keeps selection read/write independent, rejects an ordinary
window's Panel label, and checks panel revocation/display-control expiry.

The Editor-only candidate adds `test/display-control/src/native_window.rs`.
Only its Root-created private selector enables standard XDG toplevel, output
and SHM globals; existing synthetic selections retain their original dispatch.
The witness records an authenticated instance's actual configure acknowledgement,
nonuniform client-authored pixel buffer, subsequent frames/title and window
destruction outside the App sandbox. It never writes an App result or grants
input, Layer Shell, a bus, a render device or file access. Unsupported renderer
or protocol requirements remain a hard failure. This headless buffered-window
check is not physical-display/pixel-perfect acceptance.
The private `render_trace` records read-only pre-commit/pending versus committed
buffer counts, other-surface buffers, actual frame callbacks sent and output
bindings. Buffer kinds are 0 absent, 1 new and 2 removed; a current snapshot may
already be empty after the normal witness consumed it. These counters and
configure acknowledgements never substitute for a real top-level pixel buffer.

The private keyboard child module accepts only one Root-owned, regular,
bounded command at a time in this protected fixture directory. Commands bind
the authenticated instance and an already-buffered primary or single auxiliary
surface, expire within five seconds and use a strict sequence, closed chords
or at most 128 supported ASCII characters. It forwards ordinary Smithay seat
keyboard events, with modifiers released after each stroke. It exposes no
virtual-keyboard global, production control hook or App input grant. Its ack
means events were sent, never that an App action succeeded.
The auxiliary slot is bounded to one normal same-instance toplevel for the
original chooser; it is not another installed identity or permission union.

The actual Calendar lane also observes one real Layer Shell surface, under
the same Root-created selector and existing production authority filter.
Only a live `ClientOrigin::Instance` with `ui.panel` can create or commit it;
namespace and App labels remain observation, never admission authority.
Its distinct typed panel record requires a configure acknowledgement, original
bounded applet geometry, real nonuniform SHM pixels and role destruction.
Ordinary XDG window bounds and generic synthetic overlay cases are unchanged.
The panel never receives keyboard focus. The Calendar-only successor reuses
the protected, finite input-command pattern for two ordinary seat pointer
clicks: original panel open and close, always at the center of its actual
already-buffered configure geometry. No caller coordinates, virtual-pointer
global or input capability is added. Replays, expiry, keyboard actions aimed
at a noninteractive panel and pointer actions aimed at XDG windows refuse.
Each finite click releases the button and moves one logical pixel beyond the
acknowledged panel width, sending ordinary pointer leave before its frame.
This clears the original toolkit tooltip's shared hover flag; otherwise its
already-queued 100 ms task can replace the agenda with a sibling tooltip.
No timer, App action or frame is forged, and the one-agenda-lifetime bound is
retained rather than crediting a destroyed agenda's pixels to its replacement.
The new bounded popup witness waits for the real `wlr_layer_surface.get_popup`
parent assignment under the same live authenticated client/instance before
configuring or accepting SHM pixels. Its own configure acknowledgement,
nonuniform raster and destruction are separate from the unchanged XDG window
and original panel predicates. The original Calendar UI/SDK is not modified;
successful real owner-scoped reader audit and popup pixels are required by the
consumer lane, but audit does not expose returned records and no exact agenda
text rendering is asserted. Source/pure checks are not a passing actual panel
or consumer result; both remain clean-GHA-only gates.

From the OS repository root:

```bash
cargo build --manifest-path desktop/comp/Cargo.toml --locked
cargo test --manifest-path desktop/comp/test/selection-read/Cargo.toml --locked
cargo build --manifest-path desktop/comp/test/display-control/Cargo.toml --locked
cargo clippy --manifest-path desktop/comp/test/display-control/Cargo.toml \
  --locked --all-targets -- -D warnings
```

The production build requires its normal native development libraries.
Headless dispatch success does not claim real greetd/logind/KMS, visual,
X11, CopyQ-history or every native producer's permission acceptance.

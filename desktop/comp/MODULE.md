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
The internal Root selection command accepts only an epoch/owner-bound,
unexpired, sealed and bounded MIME payload over the protected supervisor
channel. Its server-side source is separate from Xwayland source identities
and ordinary App clients; Xwayland clearing cannot clear an unrelated Root
source. Transfers retain existing read authorization and use bounded
nonblocking workers. This does not grant a login client selection authority.
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
records the live target's actual buffer-commit counter at final event delivery,
before the compositor dispatches subsequent client commits. This means events
were sent, never that an App action succeeded. A later-frame witness must exceed
that delivery counter; a buffer from asynchronous startup is not an input result.
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
already-buffered configure geometry. The panel accepts no caller coordinates.
Replays, expiry and keyboard actions aimed at a noninteractive panel refuse.
The Clipboard successor additionally permits four finite ordinary clicks in
that same instance's single Layer-parented popup. Its current configure must
be acknowledged with no pending configure, matching real pixels and coordinates
inside the bounded popup; the panel still permits only open and close.
These exercise delete, the separate clear confirmation and the Root restore
candidate; the latter also checks the actual server-side selection and rich
MIME transfer independently of CopyQ's offscreen data connection.
No popup keyboard focus or production input capability is added.
The Launcher candidate separately records one original Top-layer palette with
Exclusive keyboard interactivity and a 1024x800 evidence envelope on a private
1024x768 output. Only the closed Root-owned fixture expectation selects this
role; neither namespace nor App identity enables it. Existing buttons/readers
retain their None keyboard role and previous bounds. Production `ui.panel`
admission, independent selection rights and checked retirement are unchanged.
This initial lane covers original palette startup/pixels and retirement only,
not Layer keyboard delivery, catalog/search, typed application launch or
general backend-resource admission.
Autosized Layer requests receive real bounded resize configures, not just the
initial bootstrap size. The witness tracks issued serials and requires the
latest acknowledgement before crediting another buffer. Pixels accompanying
a valid resize request are released normally but cannot satisfy that pending
configure; pixels before the first acknowledgement still refuse. Existing
reader geometry and keyboard checks remain strict.
The genuine Store successor additionally
permits one finite ordinary click inside the fixture's already-buffered,
configure-acknowledged 800x600 primary XDG surface. Canonical integer coordinates
outside that geometry, auxiliary windows, unconfigured/unbuffered targets or a
different authenticated instance refuse. No virtual-pointer global, production
control hook or App input capability is added.
Each finite primary click releases its button and keeps the ordinary pointer
inside the selected surface. Batching `CursorLeft` with a primary click leaves
the client's event batch with an unavailable hit-test cursor, so delivery and
a later frame alone do not prove result-card selection.
Each finite panel click releases the button and moves one logical pixel beyond
the acknowledged panel width, sending ordinary pointer leave before its frame.
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

The Calendar-only synthetic fixture can arm a passive SHM evidence selector
owned by Root and bound to that exact instance. Only its current live,
Layer-parented, configure-acknowledged popup is sampled. Visible pixel bytes
are copied once alongside the existing checksum calculation, without pool
padding or unrelated SHM contents. A single `native-calendar-agenda.shm`
container atomically couples immutable JSON metadata and packed native
BGRA/BGRX bytes; each real popup buffer attachment replaces it, through
ordinary close.
Publication is capped at 32 frames, 4 MiB of pixels per frame and 32 MiB total.
Limits/I/O/unsupported formats invalidate the optional evidence, not the
existing acceptance gates. Final window metadata distinguishes the latest
captured live commit from unmap/close or a later unmatched configure.
A transient resize buffer/configure mismatch removes stale capture evidence,
but a later matching acknowledged buffer may be captured. It never resets
publication/byte counters or revives an I/O, format or quota failure.
There is no new App input, capture or accessibility grant, render alteration,
service reply hook, OCR or text claim. A snapshot can still precede App reply
processing; Parent's later visual inspection is separate evidence.

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

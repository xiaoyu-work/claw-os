# Authenticated GUI Host

`gui/` owns the Root-supervised lifetime of one verified App GUI. It does not
introduce a permission store or treat a native executable, publisher, App ID,
desktop label or Panel presentation as authority.

## Contracts

Registration uses the existing App session authority. Optional `desktop.needs`
selects only the default GUI's own needs; an explicit empty array requests none.
Without that field, the constrained union of fixed/wild **operation** needs
retains its legacy semantics. A named `desktop.entries.<name>` always declares
its own needs, never a union with operations, MCP or another GUI. Fixed needs
outside launcher delegation use the ordinary exact-owner approval flow unless
the owner already saved a supported exact choice or the OS preapproved this
build and its direct GUI declaration. A signature or GUI declaration alone grants nothing.
Wild needs still inherit only the launcher's bounded scopes.
Argument-bound GUI needs are unsupported/unbound. Explicit conditional GUI
needs are also unbound, not unconditional grants; the legacy operation union
still does not evaluate `when`. Neither binds a later file-picker selection.

The installed system desktop shell is started by the authenticated Root
display supervisor, not by an ordinary App launcher. Only its direct launcher
and verified vendor-signed default entry qualify for the fixed desktop startup
policy. That policy covers the declared panel, window controls, notification
presentation and current-display lock, plus supported optional device controls.
It still checks App denials, package trust and instance lifetime. It grants no
power, App-launch or Agent capability. The signed system menu dispatches an
explicit owner click through its private endpoint, pinned display and exact
signed launch target or logind provider. The default vendor-signed `claw-launcher`
window started by that supervisor may submit a selected signed desktop entry
through the same gate; it cannot start the Agent, issue power actions, or
spawn itself. Its overlay still requires its own `ui.panel` admission. Other
GUIs retain exact per-action reviews; sleep still requires an acknowledged
native lock.

The private `gui.desktop.events` connection sends the system Shell one initial
window/workspace snapshot, then a new snapshot when the compositor or GUI Host
reports a change. Only the live Shell with `sys.observe:desktop` can see this
view. Running Dock entries come from managed GUI instances and their verified
desktop-entry resources on the same display epoch, not from a window count.
Other GUI clients of this connection receive only their own `reopen` event.
A Dock click can ask an already running, listening default GUI of the exact
signed target to reopen; it cannot start a second copy, send arguments to
that instance, or carry authority into a different session.

`desktop.optional_needs` and each `desktop.entries.<name>.optional_needs` are
independent best-effort feature lists, empty when absent. Missing, denied,
ask-unapproved, unbound, package/policy-refused or unsupported optional rights
do not block startup and never create an approval request. Mandatory `needs`
remain strict, including required `ui.panel`; optional declarations do not
subtract from them or from the default GUI's legacy operation union.

Root first settles the complete mandatory plan, then admits optional caps
from the authenticated parent, a supported saved owner choice, an applicable
OS default for a direct GUI launch, or a still-live exact reusable approval.
Reusable admission spends one bounded use, never a once
grant or a restoration receipt, and retains its expiry and revocation checks.
Optional Wild requests retain the existing bounded parent-only inheritance;
they never scan unrelated approved scopes or other GUI/MCP declarations.
Activity confirmations, Workspace limits, App denials, package ceilings and
the normal protected-resource/mount checks still apply. A disallowed optional
mount is omitted, not made available by weakening the resource gate.

Registration returns `optional_needs_ungranted`, zero-based indexes in the
selected surface's optional list. Root projects the same startup snapshot as
`COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON` when the selected list is nonempty.
An index can also mean only part of an inherited bounded set was available.
This presentation data is not authority or feature readiness: live
policy/provider checks still return ordinary denial errors, and AI consent,
budgets and provider availability remain separate. Existing sessions never
widen after a later approval. Expiry/revocation still uses checked instance
retirement. The bounded selected-file-read path below issues separate
operation-bound authority, never a dynamic widening of this startup snapshot.

The default GUI uses `desktop.entry`, falling back to top-level `entry` and the
runtime default. Neither override changes ordinary operations or `mcp.entry`.
Additional entries require a package-relative `entry`, literal `exec`, localized
`name` and explicit `needs`. Names match `[a-z][a-z0-9_-]*`, at most 64 ASCII
bytes; there are at most 64 additional entries. Only
`cos app <id> --gui:<name> [literal args...]` selects one. Default `--gui` and the
exact existing `desktop.exec` remain supported. No user-supplied path or
executable is interpreted.
Default and named launchers without `COS_SESSION` use the existing authenticated Root
registration directly, not an Admin/bootstrap parent. Root independently
retains their actual registered ancestry or bounded unregistered-launcher
policy; unsetting an environment variable cannot erase App ownership.

Root retains the selected name, package snapshot, entry and selector before
authorization and checks the same binding at launch. Every selected target
uses the generic signed regular-file/executable admission and descriptor-pinned
sandbox path. The launch request cannot change its GUI/MCP/entry/runtime.
The ordinary GUI CLI can wait for the broker's exact activation and capability
reviews, then retry registration as the same caller. It cannot confirm either.
All surfaces keep the same App/data/audit identity and per-App mutation fence.
The protected review lists every target and its needs/purposes, including
unbound requests. New fields enter its comparison digest, not approval authority.

For Binary, Node and Shell runtimes, [argument preparation](../../bridge/gui_args.rs)
appends `[selected.exec, ...user_args]` after any interpreter/script arguments.
The selector must equal the selected verified declaration's `exec`; it is not
rewritten, split as a shell command or required to name an operation key.
Python loads the selected signed file through its existing `run(selector, args)`
wrapper instead of receiving native argv.
The environment contains `COS_APP_GUI=1`, `COS_COMMAND=selected.exec` and
`COS_ARGS_JSON` encoding **only** the user arguments. Empty user arguments,
spaces, quotes and metacharacters remain literal.

Selectors are limited to 1024 UTF-8 bytes, must not be whitespace-only and
cannot contain control characters. There are at most 128 user arguments, each
at most 4096 bytes under the broker's bounded Text rules; their encoded JSON
must fit 64 KiB. The launcher checks these bounds before session authorization.
Root validates the selector during registration and independently checks user
argument bounds when decoding the launch request, before authorization.

Locale categories and `LANGUAGE` remain presentation. GUI execution does not
retain the tool worker's forced `LC_ALL` when the launcher left it unset;
stdio workers keep their existing deterministic environment.
The existing Iced renderer preference `ICED_BACKEND` is also closed presentation:
only `wgpu` and `tiny-skia` are accepted from the launcher and independently
decoded by Root. Unset retains the product's normal renderer selection.
This hint neither grants a renderer device nor changes filesystem, display,
content, Panel or other authority; other toolkits may ignore it.

Root exposes the verified package's read-only `resources/` as
`COS_APP_RESOURCE_DIR`. Its `usr/share` precedes standard system data roots in
`XDG_DATA_DIRS`, so toolkit schema/icon lookup does not depend on global App
resource activation. `COS_OWNER_HOME` supplies the authenticated owner path
for UI choices; `HOME` and App state remain private. Neither environment path
grants filesystem access, defeats protected-store checks or supplies a user bus.

### Interactive Capture

`capture::gui` retains at most one correlated request per private GUI endpoint
and eight outstanding requests across the broker. The closed
`gui.capture.control` request/status/cancel route uses ordinary typed admission,
capability-authority middleware and audit; public dispatch and MCP/Host relays
cannot install its Root-only endpoint context. Every call proves the actual
instance process/cgroup and current GUI grant. The selected signed surface must
explicitly declare screen capture, and Root spends the held
`desktop.capture:screen` capability; neither parent rights nor MCP needs select
this feature.

The fixed `clawd --capture-gui-helper` authenticates its private pre-fork Root
parent and dispatch byte, then contacts only the authenticated owner's portal
as that non-root owner with `no_new_privs`. No App receives the user bus.
The helper pins the portal's unique bus owner, subscribes before Screenshot and
checks the exact generated handle. A replacement bus-name owner cannot
acknowledge the old request's Close.
Human region, destination and clipboard decisions remain in the OS portal.
Saved paths are bounded metadata only, never App filesystem grants or mounts;
clipboard success and user cancellation remain distinct.

Start/status calls return promptly; retries keep the same data-only correlation
and immutable modal choice without replaying the portal. The human Response has
no 20-second screenshot timeout. Root clips it to the original live GUI lease
and checks grant/package/display lifetime through ordinary instance supervision.
Stop, revocation, logout and explicit cancel request bounded portal Close.
Quiescence requires the real Close acknowledgement (or a terminal correlated
portal Response) **and** checked helper exit. A killed helper, missing report,
unavailable Close or uncertain dispatch cannot acknowledge retirement: the GUI
remains pending under Root custody. Cleanup may outlive a controller's bounded
wait; retries do not turn an absent process into portal-retirement proof.

The non-interactive `system.screenshot.capture` path remains separate: no
interactive/clipboard requests, exact output-directory write authority and the
existing non-overwriting durable filesystem journal. Capture notifications use
the generic owner/source-bound Notification Service under independent
`ui.notify`, not a business-name entitlement. These source/fixture contracts do
not establish physical-display or installed-image acceptance.

The original human `--interactive=false --save-dir` GUI path uses the existing
controlled Home metadata and selected-create SDK contract, not an inherited MCP
directory grant. Its signed surface additionally declares mandatory Home
`fs.meta` and optional Home `fs.write` eligibility. Root separately reviews the
window's metadata and the exact absent output filename. The App reserves one
ordinary creation scratch, requests Screen-authorized PNG data through the same
private Capture endpoint, writes the selected scratch and requires the provider's
canonical `created` result after no-replace finalization. Startup/parent caps
never widen, and an existing/racing target refuses rather than being adopted.
The existing selected-file limit is 8 MiB; ordinary MCP's separate bounded
directory output contract is unchanged.

PNG data uses closed `image`/`read_image` actions: one bounded image per window,
at most 64 MiB retained, 128-KiB byte chunks and the same eight-request custody
ceiling. Every chunk rechecks the live exact GUI authority. The image attempt has
the non-interactive deadline; human interactive decisions still do not.
Stop/revocation clears retained bytes and the original checked portal/helper
barrier remains mandatory. Interactive saved-path results continue to grant
neither content nor output access.

### Managed playback

The private `gui.media.exchange` endpoint carries commands and replies for this
one authenticated GUI. Publishing an App's own playback state creates no
capability and cannot name another App, owner, session or executable. It is not
an owner-bus proxy. Normal public dispatch refuses the exchange; only the
common Host's checked per-message process/cgroup endpoint can enter it.

The owner registry selects one running target under the existing exact
`desktop.media.observe:cosmic-player` or `desktop.media.control:cosmic-player`
caller grant. Multiple targets remain ambiguous. Root keeps at most eight
pending requests per window, obtains fresh caller authorization before releasing
a command, and rechecks it before returning a result. The native UI itself
handles each command and samples status; there is no independent MCP state
cache. Expired, cancelled or retired requests cannot become new playback
commands, and an unconfirmed outcome must not be replayed automatically.
Window retirement closes the exchange and fails retained requests. Media file
and output access remain independent native filesystem selections, not effects
of playback permission or returned metadata.

### Notification presentation

The selected signed GUI must explicitly declare the fixed named resource
`ipc.subscribe:notifications.presentation` before receiving another source's
leased display projection. Exact presentation actions and popup preferences
independently require `ipc.invoke` for that same resource. The ordinary
catalog, protected review and live authority decisions apply; neither an App
identity nor the separate `ui.panel` surface permission supplies these rights.
Own-source `data.inbox.read` and publication `ui.notify` remain unchanged.
The resource also requires this same surface's explicit fixed `ui.panel`
declaration and spends that high-risk right on every exchange; a low-risk
topic subscription cannot become a standalone all-source inbox.

`gui.notification.exchange` runs only inside this instance's authenticated
private broker context. Root keeps one presenter per owner, the existing
desktop delivery leases/DND/retry policy, and bounded connection-local handles
with fresh per-delivery tickets. It exports no owner/source selector, database,
global inbox, bus endpoint or capability. Renderer acceptance is not user
acknowledgement. Only exact active handles admit dismiss/default-ack actions;
ordinary expiry, eviction and checked retirement never acknowledge.

The existing shell descriptor terminates in the Agent-owned
`claw-notification-panel`, which proxies public cards and App-owned popup mute,
not private product libraries or owner configuration. The actual original
Layer Shell App remains under the common Host. Retirement fences exchanges,
checks process/cgroup and compositor teardown, then releases presenter custody;
failed cleanup cannot silently admit a replacement. No Agent task/UI is needed.
Private native cases 42–43 exercise real signed popup pixels, the descriptor,
durable delivery/actions and checked retirement; they run only in the
disposable authenticated-display fixture, not on a shared live desktop.

### Output-only audio

`gui.audio.control` accepts only bounded stereo S16LE PCM at 48 kHz through
this window's private broker. The selected GUI must explicitly declare
`device.audio:output`, and normal authority middleware spends the held grant.
There is no caller-selected device, rate, program, bus, PipeWire graph handle or
recording mode. The App receives no owner sound-server or microphone descriptor.

The fixed `cos __gui-audio --pcm-stdin` transport uses ordinary correlated,
one-request broker calls, not extra frames on an answered connection. Root
retains at most eight output helpers and a bounded queue per stream. Its fixed
unprivileged `pw-cat --playback` child consumes only PCM stdin in the
authenticated owner's sound session. Sequence checks reject replayed blocks.
The original GUI grant and producer pidfd stay live throughout playback;
pause does not become an arbitrary short tool timeout. Stop, producer death,
revocation and logout close the relay, and GUI quiescence waits for observed
helper exit. A silent thumbnail decoder disables its audio sink instead.

### Human file picker

`gui.file.picker` presents the installed OS GTK file chooser for one live
authenticated GUI and returns one human-selected local URI as metadata. The
selected GUI must explicitly declare and hold `ui.prompt`;
ordinary live authority checks spend that existing presentation capability,
not a filesystem permission. Public and MCP callers cannot install its
private instance context. An App-supplied filename or URI is never permission.

Open/status/cancel retain immutable options and correlation, the original
GUI lease, eight bounded portal slots and the shared protected installed
`clawd` helper. Completion requires correlated portal completion or checked
Close plus observed helper exit; uncertain cleanup remains under Root custody.
The helper pins the GTK chooser's bus identity and installed executable.
The chooser does not wait for screenshot or screencast backends. Cancel and
closing the dialog end the request without a selection. When this GUI has a
controlled native view, Root also pins the selected regular file or absent
save target. The same instance can claim that evidence once for its matching
declared read, write or create operation. Folder selections do not supply
content authority. Other consumers retain their separate operation and secret
requirements. No App receives an owner-bus connection.

### Filtered Home metadata

A signed surface's explicit unconditional mandatory `fs.meta` need for the
authenticated owner's `Home/**` or one literal `Home/subdirectory/**` may select
the read-only filtered native view. The exact mandatory capability, not a broader
Home grant or optional declaration, binds that one directory.
Legacy operation unions, optional needs, App identity and broad parent rights
do not prove this window's browsing consent. A matching saved `fs.meta`
choice or OS preinstalled default avoids a repeat question; without either,
`supervision::admit_home_metadata` uses the ordinary protected exact-window
review and the CLI retries the same registration after consent.
The manager preflight call is required before Starting; the supervisor's
second admission check is fail-closed and cannot replace that retry point.

The supervisor either binds the applicable policy within the current GUI lease or
spends the window decision once and retains its exact expiry and generation.
The original GUI capability remains in the
live grant; only raw Home bind derivation is suppressed. The filtered mount
precedes private App-data binding. When that binding would otherwise sit
under the filtered root, Root gives the same pinned private directory the
namespace-only target `/run/cos/app-data`, updating private HOME, COS_DATA_DIR
and beneath-data work directories together. Toolkit icon/cache/config access
then avoids filtered Home ancestor walks. The persistent source, owner, mode,
identity, grants and provider checks are unchanged; no data is moved or copied.
Metadata subroots not covering private data keep their existing layout.
HOME and App state remain private,
and independent/broad Home content mounts still refuse rather than exposing a
raw directory or inventing a selected-content grant.

A protected outer setup gate lets Root bind the exact normal-UID runner and
checked instance cgroup before bubblewrap can inspect the FUSE source.
The source pins retain both authenticated Home and the exact declared directory.
Subroots must exist, be owner-owned directories and resolve without symlinks,
protected roots or aliases. Distinct overlapping or multiple mandatory metadata
roots refuse rather than widening the one-view node API. Inode 1 is precisely
the declared directory; Root-held node/candidate names remain relative to
authenticated Home, not relabelled relative to the subroot. Uncontrolled
Home-child metadata still refuses before raw mount derivation, including absent
paths. A separate `ui.panel` declaration changes no filesystem eligibility.
Releasing setup does not release App code: the existing inner App gate still
waits for transport, live grant and creator installation. Every metadata lease
renewal follows a fresh registration snapshot and exact window-consent check;
the bounded lease cannot outlive its original approval. No production content
watch API or mutation fault injector is promoted with metadata support. The
filtered proxy mount is write-capable only so later exact write selection can
operate; its default file permissions and callbacks still deny writes.

### Draft selected regular-file read

`selected_read.rs` admits only a proposed virtual inode from this window's
controlled metadata view. Typed request/status bodies carry no App-selected
owner, source path, window, lease or approval flag. Root authenticates the
live process/start and exact instance cgroup, then pins the source's raw bytes,
device/inode and revision and checks this signed surface's own read declaration.
Other GUI/MCP needs and parent `fs.read` are never selection. Unsupported
argument-bound needs and missing Wild inheritance remain unbound.

Selected-file declaration checks freshly verify the exact package retained by
registration, rather than rediscovering the whole installed App catalogue.
The complete signature, file contents, permissions and inventory are still
checked on every use. The retained directory identity and running package
reference must also match; descriptor pinning is not treated as immutability.
Only that freshly matched package can validate the retained manifest's needs.

An exact OS-preinstalled default held by this window or a current OS file-picker
selection for the matching mode, path and pinned revision avoids an additional
approval. Otherwise the proposal uses the ordinary exact-owner review. Each
path binds one regular file, read-only access, the retained identity/revision
and the live GUI. Root issues a separate finite read grant, preserving ordinary
package/denial, Activity and Workspace checks, without changing parent/global
or immutable GUI startup caps. The Root-held grant is `GuiResource`-only and
unindexed; no handle reaches the App. Its effective native lease is one pinned
regular file even when the declared technical upper bound is Home-recursive.
Non-UTF8 names use opaque node identity and escaped inert review display, not
lossy paths.

`pending`/accepted is not `selected`. The fixed read deadline is clipped by its
own consent, metadata and GUI ceilings, including any shorter ordinary grant.
Status polling and metadata refresh never renew or extend that deadline.
Live authority/source checks govern native opens/reads; independent expiry and
ordinary owner/session revocation also trigger checked whole-instance
retirement for retained FDs/mappings and separate-process-group descendants.
Changed sources are stale, not silently rebound. Failed cleanup retains custody.
Checked window retirement also closes pending private read reviews and retires
unused approved decisions, preserving the original owner decision as evidence.

The controlled Home path suppresses all beneath-Home filesystem capabilities
from raw bind derivation, not from capability authority. Optional registration
resource validation uses that same projection only for an explicitly mandatory
Home or Home-subroot metadata surface; it neither grants metadata nor replaces
later protected window admission. Unrelated paths retain ordinary mount/protected-resource
validation. Raw Home refusal is unchanged. The controlled broker also refuses
legacy scalar filesystem read/write/replace so those routes cannot turn a broad
startup cap into selected content. Watch and general mutations remain closed.
Unsupported mandatory Home delete/watch/exec still refuses launch rather
than silently downgrading a mandatory contract to a readonly view.

Selected-file CLI primitives use only the fixed private GUI provider transport.
The mandatory Root connect mediator authenticates the actual socket peer as
this retained Root controller in the host namespace before admitting that hop.
The App's user/PID namespaces can translate host Root to an unmapped UID/PID,
so those translated values are not accepted as approval authority. The narrow
client helper cannot carry lifecycle, installation or review commands; their
ordinary Root-peer checks remain unchanged. Socket names/environment select no
authority, and every resource request still proves its live instance/cgroup.

### Draft selected write and reserved save

The same coordinator independently reviews `fs.write` for an existing regular
file. Mandatory/optional declarations remain eligibility, not selection, and a
read selection cannot become write or vice versa. A protected write decision
can authorize bounded native writes and a Root-reserved replacement of that one
target; it creates no writable siblings, directory, delete or exec authority.
`O_RDWR` requires both independently approved resources.

Root creates one capability-empty canonical mutation evidence session for this
GUI only when a write or creation decision is installed. It uses existing inverse bytes and
MAC-chained `MutationBracket` records, linking each production effect to the
ordinary selected-write grant reference. Effects wait for durable starts; file
and required parent sync plus committed completion precede successful replies.
Unknown outcomes fence and retire the instance while retaining unresolved
canonical evidence. Test fault injectors and direct prototype selectors stay
test-only.

Atomic save gives the App one opaque private scratch name under the pinned
target parent. Only that name can be created and only that existing target can
be replaced. Discarding private scratch is allowed, unlink/rmdir of owner data
and general rename are not. Non-UTF8 inverse targets fail before effects rather
than converting identity lossily. This existing-file path cannot create a new
Save As target.

The separate draft creation path accepts only one materialized directory node
(including the view root) and a bounded literal UTF-8 leaf. Its independent
protected owner/window `fs.write` decision binds the absent child and pinned
parent. A selected or reserved status is not a created file. Root supplies one
private scratch and one finalization attempt, records the genuine Missing
inverse before staging, forces atomic `RENAME_NOREPLACE`, syncs the file and
parent and commits canonical completion before reporting `created`.
An existing or racing target is never overwritten or adopted. Creation grants
neither content read nor subsequent overwrite; unknown effects retain custody
and fence the instance. See the [native creation contract](../../worker/native_fs/MODULE.md#one-absent-regular-child--save-as).
These draft interfaces do not complete installed Editor/Files UI acceptance or
publish a new SDK/platform.

The separate `native-store` candidate lane uses the original signed Store ELF,
installer resources and published platform pin. Its launcher has invocation
authority only; ordinary `sys.observe:packages` startup review is independent
of the ungranted optional Critical package feature. Private canonical standard
catalog/inventory data goes through the real SDK/Root readers without raw state
or bus mounts. The unchanged startup bound requires configured nonuniform client
pixels and real read requests, followed by checked whole-instance/PAM retirement.
This first gate does not assert reply decoding, exact painted entries or the
native GUI's payload self-handoff; those remain separate acceptance work.
Its separate bounded ordinary-key search phase may retain the current
authenticated own-instance top-level SHM bytes for inspection. The fixed Store
and Calendar snapshot selectors/roles remain distinct, share only the existing
finite visible-row copier, and confer no App capture or input authority.
The same ignored Store test has a separate private APT round, with its own
PID/mount/network namespaces and reserved NSS fixture. It packages and verifies
real native Store dpkg ownership using the correct native architecture, then
uses ordinary search, a result-card click and the installed entry's uninstall
button and confirmation. The original signed UI must create the exact
operation-bound Critical review, visible through the ordinary owner's CLI,
without changing package state or the startup grant. The controller supplies
one protected exact Critical decision, then ordinary scrollbar/continuation
input must hand the actual native payload removal to Root. Any unknown-effect
all-App stop needs its separate protected review. Only a durable applied Root
record, one real successful APT child, checked caller/provider retirement and
ordinary-owner read-only reconciliation outside the App establish this mutation gate; input
delivery, Critical approval and accepted receipts are never package success.
The Store lane of Test run `43360457451`, at source
`d18c4dc5cba0227662f46a88a4534b70584410c7` with immutable App source
`8aa39c6ea15d39b3c2537610d9a7cdd6ee1c14bd`, passed both isolated rounds.
Its exact Remove request reached confirmed applied completion with one APT
child exiting zero, the delivery hook consumed, real Store payload removed
and separate support retained. All retained process pidfds, instance cgroup,
runtime/provider mount, Wayland window and PAM lifetime passed checked
retirement. Ordinary-owner recovery and invocation lookup changed neither
dpkg state nor the retained child list. This establishes only that actual
Store self-handoff, not all native/resource acceptance or release readiness.
Native failure evidence may also sample only the validated synthetic App's
current metadata pathname argument on x86_64, discard it if the syscall changes,
and report a fixed path-environment allowlist and bounded Home/data mount rows.
The existing Files lane also records at most eight such samples, three seconds
apart before its unchanged first-frame deadline, and a passing-frame summary
with IO counts and the same path/mount facts. Path samples are at most 512 bytes
with explicit truncation; no successful readiness or performance fix is inferred.
This is private clean-CI diagnosis, not an App memory, path or inspection grant.

The same ignored native panel fixture also has an explicit Widget Rail mode,
selected by a protected Root marker in its own disposable namespace. Its real
signed ELF must settle independent Calendar, task, resource and `ui.panel`
owner reviews without a model or inherited MCP rights. Existing OS readers
must complete, with no raw Calendar database or owner/system bus in the App.
A subsequent configured, nonuniform panel frame may use a distinct
instance-bound passive snapshot profile under the same finite copy budgets.
The private compositor accommodates panels up to 512 by 800 pixels; Calendar's
original 512-by-128 readiness and pointer predicates remain unchanged.
Test run `43374891104`, at Root source
`94ba54b410e8a601ad1c93464d52917150974483` and immutable App source
`51a10f6c9f90332bbf4f1d07d5b816b650861ca2`, passed all fifteen gates.
The original Widget Rail produced real 360-by-628 SHM pixels and three
successful exact-scope Root reads, followed by checked process/cgroup,
provider/runtime, window and PAM retirement. Manual inspection of those
same-instance bytes confirmed the synthetic Calendar event, empty task state,
memory/storage and deterministic suggestion cards. CPU/network warm-up
remained unavailable first samples, not fabricated measurements.
Automated reply-decode/text assertions remain false; manual inspection is
separate evidence. Header contrast, hardware/default-renderer acceptance and
other native/provider surfaces remain separate from this bounded gate, which
does not clear full release readiness.

The Clipboard candidate reuses that ignored panel fixture in a separate
private mode. Its startup snapshot and ordinary owner reviews share one
ordered expected-needs definition: history read/write, independent selection
write and `ui.panel`, never Calendar, Agent or MCP rights. A real protected
system CopyQ process owns only seeded synthetic history outside the App
sandbox; the original popup must read it through Root and close normally.
The provider must survive App retirement and then stop explicitly.
This fixture deliberately uses an offscreen provider. It does not establish
live selection Restore, active display/provider binding or complete Clipboard
acceptance. Its distinct Root-bound popup capture has the same finite budgets;
no raw history, user bus or App-owned CopyQ execution is admitted.
The private witness compares the live Root origin's independent selection
flags to an exact Root-owned fixture projection, including ordinary pointer
delivery as well as panel creation and popup commits. History read is not selection
read; Clipboard's already reviewed selection-write need must not be mistaken
for an unexpected union or silently removed. Other native fixtures retain
their empty-selection expectations. This projection observes existing grants,
never supplies them, and no product name selects production authority.
Native fixtures stage the same OS-owned shared icon data as Agent packaging,
not an App-local icon or a replacement renderer. A missing shared theme must
not be credited as a nonuniform icon frame. The theme remains separately
owned from each product's signed resources and grants nothing.
The fixture's read-only icon overlay retains inherited system themes without
rewriting or making the package namespace's protected `/usr/share` writable.
Clipboard also retains one bounded, current open-popup frame before ordinary
close, with a distinct checked export receipt. Clearing private presentation
state during close can repaint an empty history; the final closed frame must
not be mistaken for the open reader result. Neither capture asserts text or
reply decoding, and the original UI's clearing behavior is unchanged.
The clean Clipboard lane in [run 43404862443](https://github.com/xiaoyu-work/claw-os/actions/runs/43404862443)
verified the original App `92f389d6` against the published platform: one actual
47-ms history read, independent CopyQ lifetime, popup open/close and whole
GUI/PAM retirement. Manual inspection of the same-instance pre-close frame
confirmed both public synthetic history texts at 360 by 613, configure 9,
commit 1, checksum `6602576b23f2b603`. This is separate from the unchanged false
automated reply-decode/text flags and does not close live Restore, delete/clear,
display/provider binding or publication readiness.
The continuation in [run 43412198729](https://github.com/xiaoyu-work/claw-os/actions/runs/43412198729)
also verified ordinary popup delete and confirmed clear against that provider.
The five actual list/remove/list/clear/list calls spent respectively history
read, read/write, read, write and read. Independent CopyQ queries confirmed two,
one correct remaining item and zero; opening confirmation did not mutate
history. The provider retained zero items past GUI retirement, then stopped.
Selection rights remained separate, and offscreen CopyQ still does not
establish live Restore or display/provider binding.
The Root restore successor uses offscreen CopyQ only as the pinned history
data source. Its closed script extracts the matched standard MIME data without
calling CopyQ selection APIs. The broker spends the original compound history
read/write and independent selection-write grant once, then submits a sealed
payload to the caller's authenticated display (or the sole authenticated owner
display for a non-login service caller). Missing or ambiguous targets refuse.
The protected compositor installs an OS-owned selection and acknowledges the
effect; acknowledgement loss is indeterminate, never success or replay.
The Clipboard lane in [run 43431495753](https://github.com/xiaoyu-work/claw-os/actions/runs/43431495753)
verified original UI Restore, actual Smithay selection state and plain-text/HTML
descriptor transfers. Its six successful calls separately spent history read,
the original restore compound including selection write, history read/write,
read, write and read; history counts were two, two, one and zero. The normal
GUI/PAM/resource retirement and independent CopyQ lifetime still passed.
Physical-display acceptance remains separate. No App-owned collector,
privileged identity or raw socket fallback is introduced, and completed
clipboard data belongs to the display, not the GUI.

The separate candidate acceptance selector
`clawd::server::gui_fixture::native_products::private_native_editor` requires
the real immutable App-owned Editor release ELF, original installer resources
and licenses, prepared against the published platform pin. It uses the unchanged
manifest, private fixture signing/activation, actual normal-owner PAM login and
common GUI Host. A protected compositor witness must observe a configured,
buffered window while the startup file's independent Root read review is still
pending; only a separate owner decision may produce the actual loaded-tab title
and another rendered frame. Read revocation must acknowledge actual process,
window, cgroup, native provider and runtime retirement. Neither a title alone,
SDK status nor a probe marker satisfies readiness.

This is a pending clean-GHA proof, not passing product acceptance or publication.
Exact source `2f33a5e1a` passed all ten Test jobs, including the actual signed
Editor from immutable App source `cc75d4f1` and the public platform 0.2.1.
The hardware-free CI explicitly selects the real binary's already compiled
`tiny-skia` backend through ordinary presentation. It verifies authenticated
configure, nonuniform 800-by-600 SHM pixels, an independently reviewed startup
file load and checked process/provider/window/PAM retirement. It neither replaces
the ELF nor manufactures pixels. Actual Save As, other native products, the
normal WGPU path and physical-display acceptance remain separate.
On a rendering deadline failure, the fixture records the original witness,
then uses existing checked retirement before the ordinary owner CLI receives
its normal bounded stdout/stderr outcome. Only then may a JSON-escaped 16-KiB
launcher-log tail enter the failure diagnostic. The original 25-second readiness
deadlines, real-buffer predicate and consent order remain unchanged; cleanup
never turns the failed gate into success. Renderer/protocol problems require
separate evidence, not bus/device/Home binds or forced fake parents.

The Editor-only `editor_save_as.rs` UI helper follows those accepted startup
gates: ordinary Ctrl+T creates an untitled buffer, typed keys author its text and
Ctrl+S opens the original Git-pinned chooser. Its own rescan focuses the filename
field, whose Enter action submits Save As. The helper requires a real auxiliary
chooser frame, an exact independently pending Root creation review, actual
durable authored bytes and a verified committed Missing inverse. A subsequent
real UI save must request a new existing-file write decision; leaving it
unapproved must preserve the created bytes and single mutation. Checked
retirement must close that pending review and preserve the result.
Neither keyboard delivery acks nor chooser titles/pixels alone are App outcome
evidence. The original first-frame, read-consent and 25-second startup gates
remain unchanged; Files does not invoke this helper. Chooser rescan/focus
ordering and the full UI route require genuine clean-GHA execution, not a
source-level Save As success claim.

Only verified committed own effects can advance matching selected resources.
They preserve existing read consent and all original deadlines. In-place
changes retain same-object semantics. Atomic replacement uses a new native
inode: new opens read the new object, old read FDs/mappings retain the old object,
and detached old write FDs refuse further effects. Retained scratch reads its
own authored bytes, never later target content. Pending proposals do not follow
own revisions; external changes/conflicts still fail closed.

The Root [display registry](../../display_session/MODULE.md) supplies the
authenticated owner and display epoch. The Host creates the instance ID, holds
the actual launch-gated child pidfd and mandatory cgroup, and installs one
creator lease before releasing App execution. The authority alias/revision,
expiry and rights come from the bound live grant, not App JSON or Wayland
security-context strings.

`clipboard.read:selection` and `clipboard.write:selection` are independent
checks. History, display and Panel metadata imply neither. Zero GUI needs
therefore give zero direct selection rights. Layer-shell is a separate lease
field derived only from `ui.panel:Wild` in that same bound live grant.
`ui.panel` requires explicit authorized delegation or exact protected owner
approval; ordinary `ui.window`, native origin, signatures and Panel labels do
not imply it. The system Agent and unregistered-launcher baselines deny it,
and developer/unsigned content cannot hold it. All signed Apps use this same
gate. The launch reply and bound audit event retain the actual installed right.
Its Layer-shell surfaces may request their own overlay keyboard focus, not
global `ui.input`, virtual keyboards, clipboard selection/history, capture or
App orchestration. Unrelated compositor protocol denials remain unchanged.

Every renewal rechecks package/instance validity, process identity, the existing
App grant and App deny gate, the inherited portion of authenticated parent
delegation and its existing approval revocation generation. Exact owner-approved
needs are not silently reclassified as inherited parent rights. Authority changes retire the instance rather
than widening a cached lease. The existing App-session grant expiry still
applies; no permanent GUI grant or Admin bootstrap is created.

## Code and teardown

| File | Responsibility |
| --- | --- |
| `inputs.rs` | Closed launch/wait/stop and selected-file shapes, bounded node/status references and presentation-only keys |
| `manager.rs` | Reservation, exact launcher ownership, results and checked retirement barriers |
| `supervision.rs` | Sandbox/cgroup preparation, binding, creator installation, refresh and cleanup |
| `resources.rs` | Exact owner/window Home metadata consent, filtered mount, bounded lease and checked provider retirement |
| `selected_read.rs` | Exact file/window proposals and protected decisions, independent read/write/create authority and checked source/lease custody |
| `broker.rs` | Instance-local authenticated provider relay into ordinary broker authorization |
| `../capture/gui.rs`, `../capture/portal/interactive.rs` | One correlated interactive request, live screen spend and checked OS portal/helper retirement |
| `transport.rs` | Kernel syscall notification handling and the fixed connection targets |
| `proxy.rs` | Per-segment kernel writer/cgroup checks, bounded bytes/connections/descriptors |
| `output.rs` | Bounded output draining after actual process retirement |
| `retirement.rs` | Absolute-deadline blocking barriers and exact approval owner/session matching |

The socket proxy drains each direction independently. `POLLHUP` is not EOF:
only a zero-byte receive proves that direction has drained. Partial writes
and downstream backpressure retain pending bytes and `SCM_RIGHTS`; the
destination's write half closes only after draining, leaving reverse traffic
active. Real connection errors and checked retirement still end the relay.

`retire_app`, `retire_session`, `retire_owner` and `retire_epoch` return a
process-local Root broker completion result. They retire listeners/clients and confirm process/cgroup,
provider, proxy, transport and output cleanup. The App permission and Root
approval-revocation controllers await these barriers before returning success.
A session barrier also matches the Root-derived launcher grant session.
Internal grant cleanup must not recursively wait on the same job.

`desktop_review.rs` accepts one pending desktop effect per private GUI
endpoint. A GUI declares eligibility without acquiring an effect grant.
Root resolves the desktop ID through a publisher-signed installed `.desktop`
resource and asks the existing review controller for a one-use decision.
The decision binds the live source instance, display epoch, target publisher
and content digest. Expired, denied, changed, and retired requests cannot
dispatch. The Root display supervisor submits only the normal managed GUI
launch; the target's own activation review and capabilities still apply.
Power actions use the same review window with an exact power verb and action
instead of an App target. The OS Power Manager checks logind availability,
rechecks the live confirmation, and journals the action and review ID before
sending the effect. It keeps the authenticated D-Bus caller connected for
logind's reply; lost acknowledgements are not retried. Consent and submission
are not completion. Neither
the Shell nor a caller claiming a review ID inherits `sys.power`.
Reviewed desktop sleep actions wait for a confirmed native lock of the
requesting display and recheck the same one-use decision before dispatch.
If the lock or review is no longer valid, no sleep effect is sent. This gate
does not change headless power commands or external logind sleep triggers.

The private `gui.desktop.lock` route separately requires the live named
`ipc.invoke:desktop.session.lock` right. It can only request lock of the bound
display; protected display custody, password entry and PAM unlock never
enter App transport.

Filtered Home teardown also checks instance death before dropping Root source
pins, exact unmount and provider joins. Failed metadata or checked-cgroup
preparation remains retained and cannot acknowledge GUI retirement.
Successfully retired metadata custody is consumed before removing the cgroup;
supervisor drop must not reopen that already removed containment.
When logind removes the subtree first, final cleanup uses the kernel removal
result from the instance's pinned `cgroup.events` descriptor. Process death,
provider joins, and the remaining custody checks are still required.

Providers may observe the owned child's normal exit before the supervisor's
next poll. A health error therefore rechecks that exact child: an observed
exit retains its real status, while a still-running child retains the error.
Neither path skips checked retirement or turns a nonzero exit into success.

The approval `None` owner is the distinct system bucket, not every user or
UID 0. GUI instances are owner-bound, so only `Some(uid)` can match them.
Session approval-cache revocation also checks that exact owner, not just the
session string. Once an App denial is durable, a failed audit write does not
skip resource retirement; audit and incomplete-teardown errors are both
reported without rolling back the denial.

The asynchronous controller captures an absolute deadline before queuing
blocking retirement. Queue time counts toward it. A timeout is an incomplete
acknowledgement, not cancellation of the cleanup retaining custody.
Retries still enter the checked barrier when the App is already denied or
there are no remaining grants to withdraw; neither condition proves retirement.

These functions are not a standalone installer RPC. `MANAGER` is process-local,
and its absence in a standalone `cos` process says nothing about instances held
by the Root broker. Root Store commits now require this actual manager and
retire the App across all owners under the shared per-App admission fence,
alongside checked ordinary and MCP custody. Signed directory installation and
the public provenance App rollback frontend reuse that fence and retirement
through the Root directory controller, not this process-local manager.
Trust-revoke completion and independent OS-package mutation still need their
own Root coordination. Directory delivery refuses dpkg-owned replacement rather
than inventing an ownership transfer. Provenance has no `clawd` dependency.

`Scope::kill` and
`LaunchResources::kill_all` remain best-effort APIs for their other callers;
they are not evidence of GUI retirement. Failed barriers remain explicit
pending/error outcomes and can be retried.

See [GUI transport](../../worker/gui_transport/MODULE.md) for syscall and raw
socket containment. Named private IPC, unsupported descriptor kinds, X11,
Clipboard history/CopyQ, legacy ordinary-display providers and producer
permission acquisition are not silently made available by this Host.

## Focused checks

From the OS repository root on Linux:

```bash
cargo test -p cos --lib -- --test-threads=1 \
  caps::manifest apps::permission_review clawd::app_sessions \
  clawd::capture \
  clawd::system_review::presentation clawd::wire::requests \
  bridge::gui_args clawd::gui::inputs clawd::gui::output clawd::gui::proxy \
  clawd::gui::retirement clawd::app_permissions clawd::authority::store::tests::approval \
  worker::gui_transport \
  worker::seccomp::gui worker::net_broker display_session::registry::tests::spontaneous
cargo clippy -p cos --lib --bin claw-gui-runner \
  --bin claw-display-host --bin claw-display-session -- -D warnings
```

The proxy regressions execute the production relay with real pidfd/cgroup
bindings, not an authorization bypass. They require a non-Root test process
in a protected non-root cgroup with unset kernel login identity, and cover
96-KiB producer-close draining, backpressured descriptor forwarding, both
half-close directions, credential/descriptor refusals and checked stop.

The actual authenticated PAM/App/compositor fixture and installation inputs are
described in the [display guide](../../display_session/MODULE.md).
Its panel cases create actual overlay surfaces through the bound Root lease,
exercise independent read/write selection rights and checked revocation/expiry,
and refuse Layer-shell for an ordinary window with Panel metadata.

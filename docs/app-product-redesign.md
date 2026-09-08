# Product-Centric App Redesign

## Status and scope

This is the implementation plan for reorganizing the 75 bundled App identities
present when this work began. It describes a target, not completed behavior.
Existing broker, App Host, provenance, SDK, AI gate and system-service
boundaries remain in use.

The implementation unit is a working product boundary, not an old App
directory. Each completed slice is committed and published separately.

## Repository split

Product UI, business logic, MCP, upstream source, tests and product builds move
to [`xiaoyu-work/clawos-app`](https://github.com/xiaoyu-work/clawos-app).
Core Agent, App Host, authority, system services, SDK/runtime and OS package
integration remain here. Each source migration is a paired App-repository and
OS-repository commit: publish the product commit first, then pin that exact
revision in [`packaging/apps.lock.json`](../packaging/apps.lock.json).
There are no sibling-checkout dependencies or runtime download fallbacks.

| Source migration | Status |
| --- | --- |
| Mail native source, `mail-ai`, extension UI and product tests/build | Moved to `clawos-app/products/mail`; installed identity, paths and authority preserved |
| `email` | Moved to `clawos-app/products/mail/apps/email`; existing provider grants and installed identity preserved |
| `gateway-email` | Moved to `clawos-app/products/mail/apps/gateway/email`; nested installation, identity and grants preserved |
| `calendar` | Moved to `clawos-app/products/calendar/apps/calendar`; event storage and provider authority unchanged |
| `panel-calendar` | Complete native UI, assets, manifest, tests and build inputs moved to `clawos-app/products/calendar`; OS shell injects a shared policy-gated provider and retains desktop package ownership |
| `fs` | Moved to `clawos-app/products/files/apps/fs`; all fourteen MCP tools and snapshot authority preserved; native Files UI still pending |
| `docs` | Moved to `clawos-app/products/files/apps/docs`; four Recoll-backed tools, owner index state and scopes preserved; background indexing remains OS-owned |
| `search` | Moved to `clawos-app/products/browser/apps/search`; explicit provider choice, two MCP tools and exact credential/network scopes preserved |
| `web` | Moved to `clawos-app/products/browser/apps/web`; existing five-operation CLI/MCP adapter and AI gate preserved; reusable `cos-browser` engine remains OS-owned |
| `browser-attached` | App, Native Host and full MV3 extension moved to `clawos-app/products/browser`; ten tools and daemon authority preserved; extension deployment remains manual through the OS's fixed source pin |
| `exec` | Moved to `clawos-app/products/terminal/apps/exec`; six existing CLI/MCP operations and process registry preserved; native Terminal UI and shared session integration still pending |
| `container-manager` | Moved to `clawos-app/products/containers/apps/container-manager`; fourteen MCP tools, explicit runtimes, namespace/confirmation checks and observe/control scopes preserved; privileged execution remains OS-owned |
| `backup-center` | Moved to `clawos-app/products/backup-recovery/apps/backup-center`; seven MCP tools, exact data/credential scopes and destructive confirmation preserved; Restic execution stays OS-owned |
| `system-snapshot` | Moved to `clawos-app/products/backup-recovery/apps/system-snapshot`; five MCP tools, separate recovery authority and rollback confirmation preserved; snapshot index and backend execution stay OS-owned |
| `pkg` | Moved to `clawos-app/products/store/apps/pkg`; thirteen existing CLI/MCP operations and permissions preserved; native `cosmic-store` and shared service integration remain pending |
| `hardware-center` | Moved to `clawos-app/products/diagnostics/apps/hardware-center`; nine MCP tools and named hardware observation scope preserved; privileged collection stays OS-owned |
| `crash-doctor` | Moved to `clawos-app/products/diagnostics/apps/crash-doctor`; three MCP tools, bounded queries, coredump selectors and sensitive crash scope preserved; journals/coredumps/debugger remain OS-owned |
| `netdiag` | Moved to `clawos-app/products/diagnostics/apps/netdiag`; five MCP tools, exact target scopes, explicit TCP ports and probe budgets preserved; private bridge and host-network provider remain OS-owned |
| `storage-manager` | Moved to `clawos-app/products/storage/apps/storage-manager`; six MCP tools, canonical device paths and separate observation/diagnostic/mount scopes preserved; UDisks2 and no-repair checkers remain OS-owned |
| `accessibility-manager` | Moved to `clawos-app/products/settings/apps/accessibility-manager`; five MCP tools, closed toggle/filter choices and separate observation/control scopes preserved; session-bound Wayland/AT-SPI execution stays OS-owned |
| `audio-manager` | Moved to `clawos-app/products/settings/apps/audio-manager`; ten MCP tools, numeric bounds and separate observation/output/microphone/media-route scopes preserved; PipeWire/WirePlumber execution stays OS-owned |
| `bluetooth-manager` | Moved to `clawos-app/products/settings/apps/bluetooth-manager`; twelve MCP tools, exact observation/control scopes and stdin-only pairing responses preserved; BlueZ execution and owner-bound pairing sessions stay OS-owned |
| `camera-manager` | Moved to `clawos-app/products/settings/apps/camera-manager`; two MCP tools, bounded PNG/JPEG capture and separate observation/camera/exact-path scopes preserved; PipeWire/GStreamer execution, serial revalidation and non-overwriting persistence stay OS-owned |
| `display-manager` | Moved to `clawos-app/products/settings/apps/display-manager`; ten MCP tools, exact layout-read scope and explicit apply/restore confirmation preserved; COSMIC output control, backlights and owner-bound backup/restore state stay OS-owned |
| `desktop-manager` | Moved to `clawos-app/products/settings/apps/desktop-manager`; four MCP tools and separate observation/window/exact-AppID launch scopes preserved; Wayland window checks and native relaunch stay OS-owned, without App-to-App business calls |
| `location-manager` | Moved to `clawos-app/products/settings/apps/location-manager`; two MCP queries, existing location grant, five accuracy choices and city default preserved; GeoClue and offline timezone suggestions stay OS-owned and do not set the system timezone |
| `network-manager` | Moved to `clawos-app/products/settings/apps/network-manager`; eleven MCP tools, separate observation/Wi-Fi/VPN/airplane scopes and conditional exact secret grants preserved; NetworkManager execution, credentials and profile/state handling stay OS-owned |
| `power-manager` | Moved to `clawos-app/products/settings/apps/power-manager`; seven MCP tools, separate observation/critical power grants and strict confirmation preserved; UPower/logind execution and serialization stay OS-owned |
| `printer-manager` | Moved to `clawos-app/products/settings/apps/printer-manager`; five MCP tools, separate printer grants, exact source-read scope and cancel confirmation preserved; CUPS, pinned source descriptors and job-owner checks stay OS-owned |
| `user-manager` | Moved to `clawos-app/products/settings/apps/user-manager`; twelve MCP tools and exact identity/secret grants preserved; corrected OS status authorization to match `sys.observe:identities`; account state, password handling and rollback stay OS-owned |
| `security-center` | Moved to `clawos-app/products/security/apps/security-center`; seven read-only MCP tools and sensitive `sys.security:audit` grant preserved; evidence collection, report generation and App-bound authorization stay OS-owned |
| `firewall-manager` | Moved to `clawos-app/products/security/apps/firewall-manager`; five MCP tools, separate observation/control grants and clear/restore confirmation preserved; nftables execution, durable rules and owner/revision-bound rollback stay OS-owned |
| `usb-guard` | Moved to `clawos-app/products/security/apps/usb-guard`; six MCP tools, separate observation/control scopes and conditional deauthorization confirmation preserved; sysfs/udev/UDisks2 execution, protected-storage checks and owner/revision-bound rollback stay OS-owned |
| `config-editor` | Moved to `clawos-app/products/maintenance/apps/config-editor`; four MCP tools, exact target/source grants and apply/restore confirmation preserved; validators, atomic replacement and durable owner-bound backups stay OS-owned |
| `systemd` | Moved to `clawos-app/products/maintenance/apps/systemd`; seven MCP tools and exact-unit observation/control grants preserved without new confirmation; systemctl execution, before/after state and supported inverse-state rollback stay OS-owned |
| `event-center` | Moved to `clawos-app/products/events-audit/apps/event-center`; three MCP tools, sensitive event scope, bounded queries and PID validation preserved; source watchers, event records and pidfd lifetime remain OS-owned and separate from audit/notifications |
| `log` | Legacy four-tool JSONL implementation moved to `clawos-app/products/events-audit/apps/log`; file layout and read/write grants preserved without copying data; isolated App logs are not the OS audit trail and typed audit-service integration remains pending |
| `launcher` | Python five-tool App moved to `clawos-app/products/launcher/apps/launcher`; XDG discovery, exact launch/file grants and recent layout preserved; native UI/build and legacy forwarding replacement plus shared catalog/history remain pending |
| `clipboard-manager` | Five-tool App moved to `clawos-app/products/clipboard/apps/clipboard-manager`; selection read/write, exact source grant, MIME/primary defaults and clear confirmation preserved; Wayland execution stays OS-owned and native panel/CopyQ history integration remains pending |
| `gateway-discord` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/discord`; existing operations/MCP contract, nested install path and state layout preserved; explicit pinned `gateway._shared` imports replace relative lookup; authenticated connector admission/lifecycle and durable replay handling remain pending |
| `gateway-dingtalk` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/dingtalk`; outbound-only send/status, optional signing, keyword/Markdown/mentions and existing grants preserved; imports use pinned `gateway._shared`; no inbound Agent loop or new state store |
| `gateway-googlechat` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/googlechat`; outbound text/cardsV2, thread behavior and existing grants preserved; recipient remains informational and cannot retarget the webhook; Python/Rust contract fixtures follow source ownership |
| `gateway-larksuite` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/larksuite`; text/rich-post/card operations and existing grants preserved; corrected HMAC to the official timestamp-plus-secret key with an empty message; Python argument case moved with the connector |
| `gateway-matrix` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/matrix`; room-message send/status, URL escaping, transaction IDs, homeserver selection and existing grants preserved; inbound `/sync` remains unimplemented |
| `gateway-mattermost` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/mattermost`; outgoing webhook send/status, channel/DM and username/icon overrides plus existing grants preserved; Python parameter cases moved and Rust recipient binding uses pinned source |
| `gateway-rocketchat` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/rocketchat`; REST send/status, channel/DM targets, token/user-id headers and existing grants preserved; no inbound service or local state added |
| `gateway-signal` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/signal`; external REST send/status, phone/group handling and existing grants preserved; backend/account state remain external, private-network access is not automatically authorized and inbound polling remains unimplemented |
| `gateway-slack` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/slack`; Web API send/status, bot-token precedence, API-level errors and existing grants preserved; Rust manifest fixture follows the immutable source pin, and Socket Mode / Events HTTP remain unimplemented |
| `gateway-sms` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/sms`; Twilio REST send/status, Basic authentication, phone/Messaging Service sender selection and existing grants preserved; no inbound webhook, delivery callback or state migration added |
| `gateway-teams` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/teams`; default Adaptive Card, explicit legacy MessageCard and existing grants preserved; recipient remains informational, Python argument cases moved and Rust binding fixture follows the source pin |
| `gateway-telegram` | Source and existing tests moved to `clawos-app/products/messaging-channels/apps/gateway/telegram`; four operations, repeatable text, polling, allowlists/rate limits and grants preserved; offset/PID state remains installed data with OS migration, while authenticated admission and supervised lifecycle remain pending |
| `gateway-webex` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/webex`; email/room routing, Markdown/plain payloads and existing grants preserved; Python argument case moved, while person-ID autodetection remains unimplemented despite older descriptions |
| `gateway-whatsapp` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/whatsapp`; Cloud API send/status, existing Graph version, separate sender ID/recipient number and grants preserved; no webhook reception, delivery confirmation or state migration added |
| `gateway-zulip` | Source moved to `clawos-app/products/messaging-channels/apps/gateway/zulip`; stream/topic and private-email routing, API-level results and existing grants preserved; all fifteen Messaging Channels source identities are relocated, but inbound/lifecycle redesign remains pending |
| `gateway-ntfy` | Source moved to `clawos-app/products/notification-delivery/apps/gateway/ntfy`; required server/exact host, metadata and auth behavior preserved, including public-default stored-token suppression; durable Notification Service/Rust adapter stay OS-owned and no queue/state is duplicated |
| `gateway-pushover` | Source moved to `clawos-app/products/notification-delivery/apps/gateway/pushover`; exact API host, application/user keys, recipient flag, metadata and emergency constraints preserved; Python argument case moved, while receipt acknowledgement and durable OS-service integration remain separate |
| `gateway-webhook` | Source moved to `clawos-app/products/notification-delivery/apps/gateway/webhook`; JSON/raw payload, target flag/default lookup, auth precedence and independent HMAC preserved with existing grants and shared egress; argument/egress regressions relocated and Rust fixture pinned; all three delivery source identities moved, not durable-service integration |
| `gateway-homeassistant` | Source moved to `clawos-app/products/home-integration/apps/gateway/homeassistant`; REST send/call/status and grants preserved, canonical list parsing fixed for flag-shaped text/options; external server, device state and automation engine not imported, and private endpoint access is not enabled |
| Other products and connectors | Pending their individual paired migrations |

Moving source does not complete a product redesign milestone or merge legacy
identities. APT remains the installed update path until a separate, complete
distribution change replaces it.

Native desktop sources must move with their build dependencies and package
ownership. Calendar now exports its full native presentation library and
assets from the immutable App source pin. The OS `cosmic-applets` host links
that library against the forked toolkit and injects a typed agenda callback;
Widget Rail and the host share an OS-owned read-only provider rather than an
App dependency. Manual and chroot builds prepare the same inputs under
ignored build storage. The launcher and native shell stay in
`claw-os-desktop`, not `claw-os-agent`; grants and data remain unchanged.

## Product contract

An App owns a business domain, its account/object identities, state and
operations. Human UI and Agent MCP are clients of the same business
implementation. UI does not have to speak MCP, and MCP must not depend on a
window being open.

```text
Human UI -----------+
                    +--> product business interface --> product state
Agent --> App MCP --+                  |
                                      +--> controlled system services
                                      +--> SDK AI gate
```

- Product identity, process identity and installation unit are different
  concepts. A product may have several processes without duplicating its
  accounts or authorization policy.
- Sharing implementation does not merge grants. Each entry point and operation
  has its own authenticated authority; an AI-only host does not inherit mailbox
  deletion or account-management permissions.
- Apps do not invoke other Apps, including through App-owned agents. The
  system Agent coordinates cross-product workflows. Apps may use controlled
  system services and shared libraries.
- Filesystem, HTTP, execution, indexing and storage are reusable capabilities,
  not alternate business Apps for Agent callers.
- App manifests and system-service definitions remain their respective
  contract authorities. Do not create a second hand-maintained operation
  catalog for a UI transport.
- One authoritative state source per domain does not mean one database for all
  domains. Notifications, audit, context events and Agent memory stay separate.
- A removed App ID has no runtime alias or fallback. Installed user data needs
  an explicit migration/preservation decision, not deletion or indefinite dual
  writes. Renamed identities require renewed or explicitly migrated consent,
  not automatic inheritance of the union of old permissions.

See [architecture](../ARCHITECTURE.md),
[App development](app-development.md), and
[extension provenance](extension-provenance.md) for the existing enforcement
contracts.

## Complete ownership map

Each starting App ID occurs in exactly one row below. These are ownership
groups, not a proposal to replace 75 Apps with 28 new App packages.

### Business products

| Target | Starting App IDs | Implementation disposition |
| --- | --- | --- |
| Mail | `email`, `mail-ai`, `gateway-email` | One mailbox/account and sending implementation; UI, MCP, AI assistance and restricted delivery adapter belong to that product. |
| Calendar | `calendar`, `panel-calendar` | Calendar owns event identity, sync and reminder rules; the panel is a UI surface. |
| Files | `cosmic-files`, `fs`, `docs` | Files owns the file-management experience; filesystem and file indexing become reusable services, not required App-to-App dependencies. |
| Editor | `cosmic-edit` | UI and MCP share editing behavior and controlled filesystem/AI services. |
| Browser | `web`, `browser-attached`, `search` | One product interface; isolated headless sessions and attached logged-in sessions are explicit, separately authorized modes, never automatic fallbacks. |
| Terminal | `cosmic-term`, `exec` | Terminal owns terminal sessions; process execution is a system primitive. |
| Store | `cosmic-store`, `pkg` | Shared software-management interface backed by the package transaction service. |
| Media Player | `cosmic-player` | UI and MCP operate on the same playback/session state. |
| Capture | `cosmic-screenshot` | Shared capture behavior with caller-specific authorization and user-interaction requirements. |
| Backup and Recovery | `backup-center`, `system-snapshot` | One product surface, with distinct data-backup and whole-system recovery semantics and permissions. |
| Containers | `container-manager` | Container-management domain with CLI/MCP and an optional console; a new GUI is not a prerequisite. |

### Shell and system management

| Target | Starting App IDs | Implementation disposition |
| --- | --- | --- |
| Launcher | `cosmic-launcher`, `launcher` | Shared application catalog, launch behavior and shell presentation. |
| Notifications | `cosmic-notifications`, `notify` | Existing core Notification Service is authoritative; remove the separate App JSON store through an explicit state decision. |
| Clipboard | `clipboard-manager`, `panel-clipboard` | One clipboard service and permission boundary, with panel presentation. |
| Settings | `cosmic-settings`, `accessibility-manager`, `audio-manager`, `bluetooth-manager`, `camera-manager`, `display-manager`, `desktop-manager`, `location-manager`, `network-manager`, `power-manager`, `printer-manager`, `user-manager` | Settings organizes pages; independent system providers retain exact scopes. Do not create a super-privileged Settings process. |
| Maintenance | `config-editor`, `systemd` | Typed configuration and service-management operations; no standalone forwarding Apps. |
| Diagnostics | `hardware-center`, `crash-doctor`, `netdiag` | Hardware, crash and network diagnostics remain modular services consumed by diagnostics UI and Agent tools. |
| Security | `security-center`, `firewall-manager`, `usb-guard` | Shared presentation, separate enforcement for security inspection, firewall and device authorization. |
| Storage | `storage-manager` | Storage-management service with optional UI, not raw access to all product databases. |
| Events and audit | `event-center`, `log` | Query surfaces over distinct event/audit authorities; do not merge them into notification storage. |
| Agent and shell UI | `widget-rail` | Presentation of existing product/task state, not another workflow or state owner. |

### Shared capabilities

| Target | Starting App IDs | Implementation disposition |
| --- | --- | --- |
| Document engine | `doc` | Shared parsing and conversion; product-specific AI stays with the consuming product. |
| Storage SDK | `db`, `kv` | Owner/App-scoped storage; not a global App database or Agent-memory substitute. |
| HTTP | `net` | Controlled network interface with exact destination authority. |
| AI gate/helpers | `summarize` | Shared AI capability used under the consuming product's identity and budget. |

### Connectors

| Target | Starting App IDs | Implementation disposition |
| --- | --- | --- |
| Messaging channels | `gateway-discord`, `gateway-dingtalk`, `gateway-googlechat`, `gateway-larksuite`, `gateway-matrix`, `gateway-mattermost`, `gateway-rocketchat`, `gateway-signal`, `gateway-slack`, `gateway-sms`, `gateway-teams`, `gateway-telegram`, `gateway-webex`, `gateway-whatsapp`, `gateway-zulip` | Signed, optional channel connectors; admit supported inbound messages through authenticated owner/sender binding, rate limits and replay controls before the system Agent. |
| Notification/event delivery | `gateway-ntfy`, `gateway-pushover`, `gateway-webhook` | Restricted delivery adapters; reuse durable service leases, retries and acknowledgements instead of another notification database. |
| Home integration | `gateway-homeassistant` | Device-control integration, not a chat transport. Do not invent a new GUI without a product requirement. |

The additional `ffmpeg`, `libarchive`, `libreoffice` and `qpdf` adapters remain
engine/provider integrations. The adapter template is not an installed App.
Connector/adapter classification does not exempt code from package
authentication, revocation, sandboxing or capability checks.

## Open-source product ownership

For products requiring deep UI or domain changes, prefer a product-repository
source fork over a permanent parallel implementation outside the product.
The existing [desktop fork](../desktop/README.md) and its
[provenance record](../desktop/PROVENANCE.md) demonstrate the source layout,
not a blanket license clearance for new imports.

Before importing a product:

1. Record the upstream source URL, immutable revision, source digest, license,
   dependency licenses and applicable trademark restrictions.
2. Preserve notices and license files; document local changes and source
   distribution obligations for the actual imported components.
3. Integrate a reproducible build and package/update path before replacing the
   installed binary. Use the Linux filesystem for source/image builds.
4. Name the security-update tracking and patch-import process. A product fork
   may diverge from upstream features but must not abandon security fixes.

Do not copy every library into the tree merely for visual uniformity. Engines
that need no local modification can remain dependencies. External services and
closed-source products retain their documented integration boundaries.

The complete Thunderbird source is vendored in `clawos-app/products/mail/comm`.
Installed images still use distribution-provided Thunderbird with the extension
and native host until the native fork is built, packaged and accepted. Source
ownership alone does not complete that product cutover.

## Sequenced implementation

| Step | Status | Scope and exit condition |
| --- | --- | --- |
| P0 | Published | Publish this ownership map and link it from maintained navigation. |
| M1 | Implemented | All six UI/MCP AI operations call shared typed functions without argv translation. The canonical App, native launcher and versioned XPI ship together; both transports reject malformed input before effects. Native grants derive from the MCP contract and stay limited to AI/own-memory authority. |
| M2 | Source-first | Define account, folder, message, thread and draft operations inside the imported Mail product. The external-extension reference-layer experiment is not the product architecture. Prove UI and headless callers address the same account/object with explicit provider selection and per-operation grants. |
| M3 | In progress | Complete Thunderbird 153.2.0esr source imported at `clawos-app/products/mail/comm/`, with immutable source/platform pins and provenance. Source build, package output, branding, security updates and packaged-product execution remain to be completed. Do not ship a second independent Agent mailbox client. |
| M4 | Planned | Complete mailbox read/search/send and AI integration through the shared product backend. Consolidate SMTP delivery; cut over `email`, `mail-ai` and `gateway-email` to the canonical Mail identity atomically with manifests, discovery, launchers, extension identity, skills, packages, consent and data handling. Remove the old identities, not alias them. |
| D1 | Planned | Converge Files/Editor/Launcher/Terminal/Store/Browser UI and Agent paths on their owning services; remove the remaining desktop App calls. Preserve browser mode isolation. |
| S1 | Planned | Remove system forwarding Apps as their service contracts and UI/tool consumers are wired. Consolidate notifications using the existing durable service; retain audit/event separation. |
| C1 | Planned | Introduce the connector lifecycle and migrate channels/delivery/home integration with signed packages, explicit sender ownership and deterministic delivery policy. |
| F1 | Planned | Remove unused operation-to-argv bridges and obsolete identities, assets, manifests and package entries. Update the actual architecture/catalog and retire completed plan entries. |

M1 deliberately keeps the current `mail-ai` identity until the M4 identity and
data cutover can be complete. It is a preparatory shared-code slice, not an
alias, a claim of unified mailbox state, or the final product organization.
No new standalone "Agent email" product is introduced during that interval.

M1 includes framed native/MCP execution, UI request-builder contract coverage,
native-launch argument and capability regressions, a real XPI build, and
rootfs package-preservation fixtures. A live Thunderbird session and a full
installed-system package upgrade have not been exercised; source/UI integration
and installed-product acceptance remain necessary before completing Mail.

Source import and native build now precede further M2 interface work, following
the source-first product decision. Implement against the actual engine in
[`clawos-app/products/mail`](https://github.com/xiaoyu-work/clawos-app/tree/main/products/mail),
not an expanding external wrapper.
Do not design a second mailbox engine first and merely attach the upstream UI
afterward.

## Acceptance and publication

For each slice, trace UI/CLI/MCP callers, manifests, service providers,
authority/audit, provenance, packaging and persistent state before editing.
Use the narrowest existing runner covering the changed paths. Protocol work
needs actual framed requests and responses, not only imports or `--probe`.
UI changes require a real supported UI run before claiming their visual or
interactive behavior is complete.

Mail is complete only when a UI-selected message can be addressed through the
same account/object model by MCP, AI output remains a draft until authorized
to send, headless operations work without an open window, and the retired
identities cannot still be discovered or invoked.

A representative cross-product workflow is: the system Agent reads Mail,
creates an event through Calendar, then uses the Notification Service. Mail
does not acquire Calendar App invocation authority.

Commit and push each complete slice to `main` using explicit paths. Preserve
unrelated worktree changes. Do not publish a partial identity/package cutover
as a completed product migration.

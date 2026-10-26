# Nearby file sharing

Nearby sharing is an owner-scoped OS service, not an Agent conversation or an
Activity. CLI, TUI, native Desktop and authenticated Web clients share the same
broker state. An iPhone uses its built-in AirDrop; no phone App is required.
Android interoperability, Contacts Only, Apple identity extraction, remote
execution and live file synchronization are not part of this interface.

## Interface and execution plan

The closed version-1 contract is defined in `clawd-client::nearby`. The broker
exposes `nearby.status` and `nearby.control`. No request can select an owner,
executable, D-Bus endpoint, raw interface or capability set.

| Action | Contract |
| --- | --- |
| Status | Probe installation/hardware, show only the owner's current window, offers, peers and bounded transfer history |
| Open | Explicit 30-600 second window, expected probed radio mode and separate acknowledgement of possible Wi-Fi disruption |
| Discover | Discover on the exact open window; returned peer IDs are temporary locators, not authenticated people |
| Send | Exact window and discovered peer ID, 1-16 explicit absolute file paths or completed upload IDs |
| Decide | Accept/decline one exact, unexpired offer in the exact window; never an auto-accept preference |
| Cancel / Stop | Cancel one send or end the window, without claiming remote effects were undone |
| Upload | Owner-private bounded begin/chunk/finish/delete for browser-selected files, independent of radio activation |

Files are at most 128 MiB each, 512 MiB per set, and browser chunks at most
192 KiB decoded. Counts, retained history and IPC frames are bounded. Requests
and responses are closed; identifiers and names never create authority.

Implementation proceeds through one contract, broker authorization and durable
state, an isolated non-root file/protocol helper plus Owly's dedicated radio
service, four client presentations, then package/update integration. The
network parsers do not execute in `clawd` or the model worker. The same radio
lease is shared by the owner's presentations; starting another independent
client is not a reason to acquire a second radio or silently stop existing work.

## Consent and lifecycle

Starting the radio and sending files cross existing protected capability
approval. An open window does not authorize arbitrary file reads or sending to
another peer. Stop and decline grant nothing. Receive permission opens only a
bounded dedicated inbox, with a separate exact offer decision. A sender's name,
file name, certificate and payload are untrusted; Everyone-mode TLS encrypts
the connection but does not authenticate a contact.

The default visible window is five minutes. Every offer expires according to
the actual protocol deadline (normally 15 seconds). An old UI cannot accept a
replacement offer. Losing a UI connection does not create another transfer.
Losing the backend or restarting the broker marks unfinished work interrupted,
never automatically resends it. Cancellation after transmission may leave
remote effects and is not rollback. Received files are never opened, executed
or sent to an LLM automatically.

## Hardware and packaging

Owly is a separately compiled MIT dependency at a recorded immutable source
revision. Its radio process retains only network administration/raw-radio
privileges. The Claw-owned transfer helper runs without root privileges and
without an App/model session or arbitrary D-Bus access.

Shared mode depends on a compatible card and AP channel; Exclusive and Active
Monitor can disconnect Wi-Fi. All clients disclose this before activation.
Ordinary WSL, Docker and VM virtual adapters are not advertised as supported
radios. Missing hardware, Bluetooth discovery support or firewall admission
must be explicit rather than a successful-looking empty transfer.

This contract can be checked with isolated protocol and client fixtures;
actual iPhone interoperability, throughput, sleep/resume and post-transfer
Wi-Fi recovery still require the target physical radio and phone.

## Four presentations

| Surface | Entry and workflow |
| --- | --- |
| CLI | `cos share status` / `doctor`, `receive --seconds 300`, `open`, `browse`, `send PEER_ID FILE...`, `accept OFFER_ID`, `decline OFFER_ID`, `cancel TRANSFER_ID`, `stop`, `watch` |
| Claw TUI | `/share` opens the dedicated center. `O`/`I` compose an explicit send/receive window command, `D` discovers, `S` composes a peer-ID plus JSON file-list command, arrows scroll/select and `A`/`N` decides only the selected offer. `X` stops; `Esc` only detaches |
| Native Desktop | Nearby Sharing in the Agent sidebar, or `cos-agent-ui --nearby` / the Agent launcher action. Pick local files, explicitly acknowledge disruptive modes, choose a discovered recipient, confirm each offer, inspect progress and stop |
| Authenticated Web | `#/nearby` in the Agent Web shell. Select browser files first, stage them in bounded chunks, then activate a window. Receiving writes the OS host's inbox, not an arbitrary browser download location |

CLI output is structured JSON. For example:

```sh
cos share doctor
cos share receive --seconds 300
# On an Exclusive/Active Monitor card only, after reading the warning:
cos share receive --seconds 300 --allow-wifi-disruption
cos share browse
cos share send PEER_ID "$HOME/Downloads/report.pdf"
cos share accept OFFER_ID
cos share watch
cos share stop
```

Native/CLI source paths must be regular readable files inside the authenticated
owner's home, outside protected OS/credential stores. Directories, URL shares
and Contacts Only are not supported. Browser files are staged separately under
the same owner and quota. Unused/partial uploads remain visible for explicit
deletion; uncertain chunks are not replayed.

All clients use the same `NearbyView`, `NearbyAction` and `NearbyResponse`
semantics. The Web and native adapters expose:

```text
GET  /api/nearby
POST /api/nearby/control
```

The control body is one closed envelope, for example:

```json
{
  "request": {
    "action": "open",
    "duration_secs": 300,
    "receive": true,
    "expected_mode": "shared",
    "allow_disruption": false
  }
}
```

Owner identity comes from the authenticated OS peer, never JSON/query fields.
Live actions require the exact current window ID, and send/decide/cancel bind
exact peer/offer/transfer IDs. Browser staging uses `upload_begin`,
`upload_chunk` with exact decoded offset, `upload_finish`, and `upload_delete`.
No action contains an executable, raw radio selector, portable grant or
auto-accept setting.

## Implementation and acceptance plan

| Layer | Deliverable | Acceptance |
| --- | --- | --- |
| Definition | Closed schema 1 in `clawd-client::nearby`; counts, byte/frame limits and action-specific acknowledgements | Unknown authority fields, stale IDs, malformed timestamps and oversized serialized views fail |
| Broker | One owner/window/transfer service, exact existing capability review, private durable staging and owner inbox | Foreign owners/Hosts cannot mutate; imported data is non-executing and never overwrites existing files |
| Transport | Fixed non-root sandbox and Owly radio lease | Private pipe survives interrupted reads; actual TLS `/Ask` declines without a decision and transfers after exact acceptance |
| Presentations | CLI, ratatui center, native center and authenticated Web | No model/task prerequisite; visible limits, unverified names, explicit disruption, stable selection, progress and no mutation replay |
| Distribution | Independent locked builds, Root-only D-Bus acquisition, signed components and checked upgrade stop | No active-user radio bypass, raw sockets in `clawd`, automatic radio enablement or unsigned package fallback |
| Physical device | Supported card plus iPhone Everyone mode | A file each way, declined/expired offer, cancel/detach, explicit stop, sleep/resume and at least five minutes of normal-network observation |

Offer notifications are informational and expire with the offer. Delivery
still follows owner preferences, DND and ordinary retry/deduplication; clicking
or acknowledging one never accepts files. Completion/failure notifications
are published only after the corresponding local state is durable.

Outgoing progress counts reported wire bytes. Incoming live byte progress is
not supplied by the upstream receiver; completed imports report observed
persisted file bytes instead of inventing a percentage. Offered byte sizes are
sender-reported, not attestations; actual 128/512-MiB limits remain enforced.
Delivered means protocol acknowledgement, not proof that a person read a file.

## Installed behavior

Agent supplies `claw-os-nearby-v1`, both fixed binaries and MIT notices.
Desktop depends on that service interface. The optional radio daemon starts
through D-Bus, not an enabled boot-time sharing window. It retains only
`CAP_NET_ADMIN`/`CAP_NET_RAW`; the broker's address-family policy is unchanged.
Standalone `owly` packaging conflicts because it owns the same D-Bus name.

The hardware's required tools may include NetworkManager, hostapd, iproute2/iw
and Bluetooth/BlueZ. They are suggestions for headless installations, not an
implicit network-manager takeover. Desktop already supplies its usual network
and Bluetooth stack. Hardware/tool diagnostics stay explicit.

The host firewall is not rewritten. Link-local IPv6 TCP 8770/8771 and mDNS UDP
5353 need appropriate admission on `awdl0`; never expose them on the WAN.
Availability is preflight eligibility, not a successful-transfer guarantee.

Upgrade/removal stops the broker-owned window and radio before replacement;
interrupted transfers are not resumed. The MIT lease-service compatibility
fork propagates failed `Backend::down` and startup recovery rather than
publishing a false idle/restore acknowledgement. Core pins the exact service
bus owner and accepts a natural-expiry receipt only for its exact checked
lease. See [`updating.md`](updating.md) and the
[`transport provenance`](../crates/claw-nearby/PROVENANCE.md).

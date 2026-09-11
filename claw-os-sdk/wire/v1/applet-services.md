# App data services, version 1

This public, language-neutral App interface has a Rust binding,
`applet::Client`. Calendar, Clipboard and Widget Rail are current consumers,
not an eligibility list. OS providers remain in
[`claw-applet-services`](../../../desktop/applets/claw-applet-services/MODULE.md);
neither the public client nor the App contains provider implementations.

This is a separate fixed-helper protocol, not an extension of `cos --wire=1`,
a plugin ABI, an App-to-App transport or a new worker socket. The OS package
installs `/usr/libexec/claw-os-applet-provider`. Its only accepted invocation is
`--stdio-v1`, with no further arguments. Consuming packages require the virtual
package `claw-os-applet-services-v1 (= 1)`.
Stdin and stdout are private Linux pipes, as created by the SDK. The executable
uses nonblocking pipe I/O so read/write timeouts cannot leave a blocking stdio
worker preventing process exit.

## App integration and authority

Every consumer uses the normal authenticated manifest, Host, capability and
sandbox contract. App identity binds ownership, session and audit context; it
does not confer permission because it names a particular product. Language,
UI, publisher and delivery format do not select a different integration model.
The historical `applet` namespace is not a privileged App category.

The fixed executable is an OS service, not an App executable allowlist. The
client cannot establish an authenticated session, install grants, choose a
trust tier or bypass sandboxing. User-granted resource scopes still decide
each operation. Installing this ABI or a consumer package grants no permission.

## Requests

Each UTF-8 JSON object is terminated by one newline. The 4,096-byte request
limit and 1,048,576-byte response limit include that newline. All object shapes
are closed: unknown/duplicate fields, unsupported operations and invalid enum
values fail. Version and IDs use JSON integer tokens, not decimal/exponent
encodings; version fits `u32`, and positive IDs fit `u64`. This fixed-helper
family is distinct from the generated CLI schemas' mathematical integers.

```json
{"version":1,"id":1,"operation":{"kind":"calendar-day","date":{"year":2026,"month":9,"day":9}}}
```

| Operation | Additional fields | Existing OS authorization |
| --- | --- | --- |
| `calendar-day` | `date`: year `-9999..9999`, month `1..12`, Gregorian day valid for that month/year | `data.db.read:Name(calendar)` |
| `calendar-today` | None | `data.db.read:Name(calendar)` |
| `tasks` | None | `agent.observe:Name(tasks)` |
| `system` | None | `sys.observe:Wild` |
| `history-check` | `permission`: `read` or `write` | Respectively `clipboard.read:Name(history)` or `clipboard.write:Name(history)` |

There are no program, path, owner, session, scope, capability or App-ID fields.
The OS validates a complete request before any policy check or provider access.
The helper inherits its caller's existing authenticated environment and sandbox;
it neither adds authority nor opens a host/session transport. History permission
is not selection permission. CopyQ behavior and user history stay in Clipboard.
`calendar`, `tasks` and `history` name existing resources, not privileged App IDs.
`history-check` is a permission preflight, not execution of a history operation
or a transferable authorization token.
The installed helper selects `/usr/local/bin/cos` directly, ignoring `COS_BIN`
and `PATH` overrides. A successful policy response must affirm exactly the
requested verb and scope. This check does not attest a caller-selected data
directory: the OS Host must supply and confine the correct owner resource view.

## Responses

Every response echoes the request ID and declares version 1. Requests that
cannot be decoded receive ID 0. A valid request with an unsupported version
gets an `unsupported-version` error, not a downgrade.

```json
{"version":1,"id":1,"outcome":{"status":"ok","data":{"kind":"calendar","events":[]}}}
```

Success data is exactly one of:

- `calendar`: `events`, each with `id`, `title`, `start`, nullable `end`, and
  `location`; strings retain the original timezone/overlap/sort semantics.
- `tasks`: `tasks`, each with `id`, `purpose`, `status`, and `created_at`.
- `system`: `summary`, with nullable `cpu_percent`, `memory`, `storage`,
  `network_down_bps`, `network_up_bps`, and required Boolean `fallback`.
  Memory/storage use `used_mb` and `total_mb`.
- `history-allowed`: no other fields; only a successful history check.

CPU percentages are real finite JSON numbers in `0..100`, never numeric
objects or nonfinite values silently encoded as null. Optional telemetry and
event end fields default to null when absent; byte counters use `u64`.

```json
{"version":1,"id":2,"outcome":{"status":"error","error":{"code":"permission-denied","message":"History permission was denied."}}}
```

Error codes are `invalid-request`, `unsupported-version`, `permission-denied`,
`provider-unavailable`, and `provider-failure`. Failure messages retain the
existing actionable provider diagnostic. Excessive results produce a bounded
error, never truncation masquerading as successful data. Missing databases keep
the provider's original empty-result semantics; missing/broken services do not.
Calendar opens SQLite read-only, retains its 250 ms busy timeout, bounds native
records and selected serialized events to one MiB, and interrupts queries after
three seconds. Rows are filtered while streaming rather than collecting the
entire database first. Bounds/deadlines fail explicitly without partial results.
Kernel subprocess stdout is bounded to one MiB (16 KiB for policy/`df`) and
stderr to 16 KiB. Existing system telemetry fallback remains explicit through
`fallback`; task and Calendar errors never become empty success.

## Lifetime and bounds

One SDK client owns one private process/pipe; clones share it. Widget Rail uses
independent clients for independent refreshes. The system provider retains
CPU/network deltas in process memory, with the original OS-only telemetry
fallback; no sample data is sent back as caller-controlled input or persisted.

Idle pipes can live as long as the UI. Once input begins, completing the frame
has one three-second deadline; writing a reply has three seconds. SDK calls
have a twelve-second request deadline including pipe contention, followed by
at most one second of explicit error cleanup. Failed cleanup is an error, not
assumed success. A cancelled or expired mutex waiter does not stop another
clone's active request. Malformed, oversized, truncated, mismatched or timed-out
replies invalidate the client connection. Cancellation and last-client drop
terminate the private provider process group, including active kernel children;
Tokio reaps the direct child. This is not the Root GUI retirement barrier.
No failed request is automatically retried, and a stale reply cannot satisfy
a later request. The server closes on framing faults; a complete invalid JSON
request receives a bounded error and does not authorize or poison later requests.

The [Rust protocol types](../../rust/src/applet/protocol.rs),
[shared conformance cases](applet-services.cases.json) and real SDK/provider
process tests cover this versioned family. Any language can use the documented framing;
existing generated CLI/MCP wire schemas and other language bindings are unchanged.
Source compatibility does not update released SDK 1.0.0. The platform 1.1.0
contract includes this client; its actual immutable artifact, an explicit
consumer pin and OS-owned GUI/resource admission remain prerequisites.
In particular no history/selection backend,
Wayland custody, Clipboard revocation or GUI publication gate is cleared here.

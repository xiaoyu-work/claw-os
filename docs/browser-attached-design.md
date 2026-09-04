# Browser (attached) design

The `browser-attached` App drives the user's running, logged-in Chromium tabs.
Headless and isolated browsing remains the responsibility of `apps/web` and
`cos-browser`.

## Why this is a separate browser path

| Need | Headless browser | Attached browser |
| --- | --- | --- |
| Reuse the user's cookies, SSO, MFA, and extensions | No | Yes |
| Operate the window the user is viewing | No | Yes |
| Isolate an automated browsing profile | Yes | No |
| Run many independent browser jobs | Yes | No |
| Keep element references across App calls | Yes | Yes |

## Authority and data flow

```text
CLI or Agent
  -> authenticated App Gateway / App Service Host
  -> browser-attached MCP handler
  -> local policy check
  -> cos_runtime.browser_bridge
       sensitive request body on stdin, never argv
  -> hidden cos __browser bridge
  -> typed system.browser.control route
  -> clawd verifies:
       authenticated browser-attached App identity
       exact action capability
       owner uid and owner-only runtime/socket metadata
  -> $XDG_RUNTIME_DIR/claw-browser.sock
       native_host.py accepts only a root peer
  -> Chromium Native Messaging
  -> background.js
       verifies daemon-injected expected_origin
  -> content.js
       verifies expected_origin again at DOM execution time
  -> user's logged-in tab
```

The App cannot access the browser socket from its sandbox. It can submit only
the closed `BrowserControl` request schema to `clawd`. The daemon maps that
request to a fixed extension verb and injects authority-bearing fields such as
`expected_origin`, `allow_secret`, and `allow_eval`; those fields cannot be
supplied by MCP arguments.

Sensitive values, selectors, expressions, URLs, and output paths are omitted
from route audit fields. The hidden runtime bridge carries them as bounded JSON
on stdin. The audit projection retains only the session, action, and tab id.

## Wire protocol

The daemon/native-host socket and Chromium Native Messaging use the same
bounded little-endian frame:

```text
request   { id: string, verb: string, args: object }
response  { id: string, ok: true,  result: object }
response  { id: string, ok: false, error: string }
```

`clawd` generates the request id and rejects a mismatched response. Both legs
cap a frame at 8 MiB. The App imposes a separate 5 MiB decoded screenshot cap,
requires strict base64 and a PNG signature, and atomically creates a new output
without replacing an existing file, with mode `0600`.
Transport or envelope loss after dispatch is reported as indeterminate rather
than as a retryable outage.

## Capability matrix

| MCP tool | Capability | Role floor | Scope |
| --- | --- | --- | --- |
| `tabs.list` | `browser.tabs.read` | connector | wild |
| `tabs.activate` | `browser.tabs.read` | connector | wild |
| `nav.go` | `browser.nav` + `memory.write` | connector | URL host + App self-ref |
| `dom.query` | `browser.dom.read` | connector | declared page host |
| `page.snapshot` | `browser.dom.read` | connector | declared page host |
| `page.screenshot` | `browser.dom.read` + `fs.write` | connector | declared page host + path |
| `dom.click` | `browser.dom.write` | automator | declared page host |
| `dom.fill` | `browser.dom.write` | automator | declared page host |
| `dom.fill_secret` | `browser.input.secret` | admin | declared page host |
| `eval` | `browser.eval` | admin | declared page host |

`page_url` is required for every tab-content operation. Manifest planning and
`clawd` independently canonicalize it to the same `host:effective-port`
capability scope. The daemon separately injects the complete
`scheme://host:effective-port` origin, and the extension compares it to the tab
and page immediately before acting. Screenshot capture also rejects any
activation or navigation generation change during the capture interval.

`browser.input.secret` and `browser.eval` remain admin-only. `clawd` injects
`allow_secret` or `allow_eval` only after spending the corresponding exact
capability.

## Page-data handling

DOM reads never serialize current values from input, textarea, select, or
contenteditable controls. Queries and accessibility snapshots expose labels,
opaque per-document refs, sensitivity classification, and a
`value_present` boolean. Text snapshots replace populated editable regions
with a redaction marker.

`content.js` classifies password and hidden inputs, password/OTP/payment
autocomplete tokens, explicit private-data attributes, and credential-like
id/name/ARIA/placeholder metadata. Ordinary `dom.fill` refuses a classified
secret field; only `dom.fill_secret` can cause the daemon to inject
`allow_secret`.

Element refs contain a random per-document nonce and live only in the top-frame
content-script instance. Navigation invalidates them. Cross-origin iframe
automation is intentionally unsupported because it would require a separate
origin-bound capability and frame identity.

## Installed layout

| Path | Contents |
| --- | --- |
| `/usr/share/claw/extensions/claw-agent-browser/` | unpacked extension |
| `/etc/chromium/policies/managed/claw-agent.json` | extension policy |
| `/etc/chromium/native-messaging-hosts/com.clawos.browser.json` | native-host registration pinned to the extension id |
| `/usr/lib/cos/claw-browser-host` | fixed native-host launcher |
| `/usr/lib/cos/browser-agent/native_host.py` | Native Messaging/socket bridge |
| `$XDG_RUNTIME_DIR/claw-browser.sock` | owner-owned mode-`0600` socket; root clients only |

The Native Messaging host validates that `XDG_RUNTIME_DIR` resolves exactly to
`/run/user/<uid>`. It has no socket-path environment override. `clawd` verifies
the runtime directory and socket ownership/mode, then verifies the connected
peer uid. The desktop-session owner boundary assumes same-UID processes are
part of the same user trust domain; isolated App workers use separate UIDs and
cannot enter that runtime directory.

## Current platform limits

- The extension is Chromium-specific.
- DOM operations target frame `0` only.
- A stale element ref must be replaced with a new `dom.query`.
- A stable packaged extension id is required for unattended installation.

# Claw Agent — WebExtension

The user-facing half of the `browser-attached` app.  Loaded inside the user's
running Chromium (or any Chromium-flavoured browser) so the agent inherits
the user's logged-in session.

## Layout

| File | Purpose |
|------|---------|
| `manifest.json` | MV3 manifest. Permissions: `tabs`, `scripting`, `activeTab`, `nativeMessaging`, host `<all_urls>`. |
| `background.js` | Service worker. Owns the native messaging port to `com.clawos.browser`. Translates verbs into `chrome.tabs.*` / `chrome.scripting.*` and forwards to `content.js`. |
| `content.js`    | Per-frame DOM helper. Owns the per-page element ref table, executes `query` / `click` / `fill` / `snapshot`. |
| `popup.html` + `popup.js` | Toolbar popup showing live state and a STOP button. |

DOM messages are sent only to frame `0`, and the content script exits if it is
ever injected into a child frame. Element refs therefore cannot collide across
iframes or authorize actions against a different embedded origin. A random
per-document ref nonce also makes refs fail closed after navigation.

Every tab-content request carries an `expected_origin` injected by `clawd`
after exact capability authorization. It includes the scheme, host, and
effective port. `background.js` checks it against the selected tab and
`content.js` checks it again in the page immediately before the DOM action.
Screenshot capture also rejects activation or navigation changes during the
capture interval. Callers cannot supply `allow_secret` or `allow_eval`; the
daemon injects those flags only after spending the corresponding capability.

DOM read operations deliberately omit current values from editable controls.
Responses contain labels, refs, sensitivity classification, and a
`value_present` boolean, but never password, OTP, payment, hidden-token, or
ordinary typed input values.

## Loading

In production this extension is pre-installed via Chromium managed policy
(`tools/install-browser-agent.sh` writes
`/etc/chromium/policies/managed/claw-agent.json`).  During development you
can load it unpacked:

```
chromium --no-default-browser-check
# chrome://extensions → Developer mode → Load unpacked → this directory
```

Note the resulting extension ID and put it into
`/etc/chromium/native-messaging-hosts/com.clawos.browser.json`'s
`allowed_origins` array so Chromium will spawn the native host.

## Wire protocol

Same JSON envelope on both NM stdio and the AF_UNIX socket:

```
request:  { id, verb, args }
response: { id, ok: true,  result }
response: { id, ok: false, error }
```

The socket lives at `$XDG_RUNTIME_DIR/claw-browser.sock`, has mode `0600`, and
accepts only a root peer. Sandboxed Apps never receive the socket; they call the
typed `system.browser.control` provider through the private runtime bridge.

See `docs/browser-attached-design.md` for the full architecture diagram and
the capability matrix.

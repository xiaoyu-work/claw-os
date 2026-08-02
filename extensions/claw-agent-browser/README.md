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

See `docs/browser-attached-design.md` for the full architecture diagram and
the capability matrix.

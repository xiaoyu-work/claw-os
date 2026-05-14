# cos app browser-attached

Drive the user's **running** Chromium tabs from the kernel — login cookies,
SSO, MFA, and saved sessions are all reused because we don't launch a new
browser, we attach to the one the user is already in.

The pipeline:

```
cos app browser-attached <verb>
        │
        ▼  AF_UNIX  $XDG_RUNTIME_DIR/claw-browser.sock
native_host.py    (spawned by Chromium per WebExtension load)
        │
        ▼  Chromium Native Messaging  (stdio, 4-byte LE length-prefixed JSON)
extensions/claw-agent-browser  (MV3 background service worker)
        │
        ▼  chrome.* APIs + content scripts
user's tabs
```

## Verbs

| verb               | caps required                | notes                                                |
|--------------------|------------------------------|------------------------------------------------------|
| `tabs.list`        | `browser.tabs.read:wild`     | id, title, url, active flag for every tab            |
| `tabs.activate`    | `browser.tabs.read:wild`     | bring a tab to foreground                            |
| `nav.go`           | `browser.nav:host=<url-host>`| load a URL in a tab                                  |
| `dom.query`        | `browser.dom.read:host=…`    | find elements by CSS selector, return refs           |
| `dom.click`        | `browser.dom.write:host=…`   | click element by ref                                 |
| `dom.fill`         | `browser.dom.write:host=…`   | refuses if the field looks like a secret             |
| `dom.fill_secret`  | `browser.input.secret:host=…`| fills secret fields — always asks for approval       |
| `page.snapshot`    | `browser.dom.read:host=…`    | accessibility-tree summary for the planner           |
| `page.screenshot`  | `browser.dom.read` + `fs.write`| save visible-tab PNG to `--output`                 |
| `eval`             | `browser.eval:host=…`        | admin-only, always asks for approval                 |

Every verb runs `policy.require()` **before** the request is sent to the
extension, so a deny never reaches the user's tab.

## Files

* `app.json` — manifest: operations, args, declared `needs`
* `main.py`  — verb handlers, policy checks, socket client
* `native_host.py` — Chromium-spawned bridge: NM stdio ↔ unix socket

The WebExtension itself lives in `extensions/claw-agent-browser/`.

See `docs/browser-attached-design.md` for the full architecture and the
threat / trust model.

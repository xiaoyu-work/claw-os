# `cos app browser-attached`

Drive the user's running Chromium tabs while preserving their cookies, SSO,
MFA, extensions, and visible browser state.

```text
CLI or Agent
  -> authenticated App Gateway
  -> MCP-only browser-attached App
  -> private stdin runtime bridge
  -> typed system.browser.control route
  -> clawd exact-capability check
  -> root-only Native Messaging socket
  -> Chromium extension and top-frame content script
  -> user's tab
```

The App sandbox never receives the browser socket. `clawd` maps the closed
request schema to a fixed extension verb and injects `expected_origin`,
`allow_secret`, and `allow_eval` only after authorization. The extension and
content script verify the full `scheme://host:effective-port` origin
immediately before acting.

## Tools

| Tool | Required capability | Scope |
| --- | --- | --- |
| `tabs.list` | `browser.tabs.read` | wild |
| `tabs.activate` | `browser.tabs.read` | wild |
| `nav.go` | `browser.nav` + `memory.write` | URL host + App self-ref |
| `dom.query` | `browser.dom.read` | `page_url` host |
| `dom.click` | `browser.dom.write` | `page_url` host |
| `dom.fill` | `browser.dom.write` | `page_url` host |
| `dom.fill_secret` | `browser.input.secret` | `page_url` host |
| `page.snapshot` | `browser.dom.read` | `page_url` host |
| `page.screenshot` | `browser.dom.read` + `fs.write` | `page_url` host + new output path |
| `eval` | `browser.eval` | `page_url` host |

Use the URL returned by `tabs.list` as `--page-url` for tab-content tools.
`dom.fill_secret` and `eval` remain admin-only and approval-gated.

## Files

| Path | Role |
| --- | --- |
| `app.json` | Authoritative MCP tool, argument, and capability contract |
| `main.py` | Typed business operations and local policy checks |
| `server.py` | Direct `claw_os_sdk.mcp` handlers |
| `native_host.py` | Root-client-only Unix socket to Chromium Native Messaging bridge |

See [`docs/browser-attached-design.md`](../../docs/browser-attached-design.md)
for the full trust and data-flow design.

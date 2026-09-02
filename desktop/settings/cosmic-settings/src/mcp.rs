//! MCP tool surface for `cosmic-settings`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening the settings GUI. Tools:
//!
//!   - `settings.list_pages` — every page-command this build supports
//!     (e.g. `wifi`, `display`, `agent`, …). The agent uses this to
//!     ground its answers when the user asks "where do I change X?".
//!   - `settings.search` — case-insensitive substring match against
//!     the page list, so the agent can fuzzily resolve "darkmode"
//!     → `appearance`.
//!   - `settings.open` — spawn the GUI on a specific page. Use this
//!     when the user wants to do the change themselves; for changes
//!     the agent can apply directly, prefer `cos config set` /
//!     `cos profile *` which go through the kernel capability gate.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{Server, Tool, ToolResult};
use serde_json::{Value, json};

/// The full list of page-commands this build exposes. Keep in sync
/// with `PageCommands` in `main.rs` — keyed by the CLI subcommand
/// the user would type (`cosmic-settings <id>`), with a
/// human-readable label and a one-line description for the agent.
///
/// We intentionally do NOT mirror cargo `cfg(feature = …)` here: the
/// MCP server is informational, and the worst case if a feature is
/// off is `settings.open` failing with a clap parse error — which we
/// surface verbatim.
const PAGES: &[(&str, &str, &str)] = &[
    ("accessibility", "Accessibility", "Magnifier, screen reader, contrast."),
    ("about", "About", "OS version, hardware info, hostname."),
    ("agent", "Agent", "Default LLM provider, memory, approvals."),
    ("appearance", "Appearance", "Light/dark mode, accent colour, theme import/export."),
    ("applications", "Applications", "Installed app preferences."),
    ("bluetooth", "Bluetooth", "Pair, unpair, manage Bluetooth devices."),
    ("date-time", "Date & Time", "Timezone, clock format, NTP."),
    ("default-apps", "Default Apps", "Default browser / mail / image viewer / ..."),
    ("desktop", "Desktop", "Wallpaper-less desktop, hot corners, animations."),
    ("displays", "Displays", "Resolution, refresh rate, scaling, night light."),
    ("dock", "Dock", "Position, autohide, size."),
    ("input", "Input", "Keyboard, mouse, touchpad master switch."),
    ("keyboard", "Keyboard", "Layouts, repeat rate, shortcuts."),
    ("legacy-applications", "Legacy Applications", "X11 application compatibility."),
    ("mouse", "Mouse", "Pointer speed, acceleration, scroll direction."),
    ("network", "Network", "Connections overview."),
    ("panel", "Panel", "Top bar size, position, applets."),
    ("power", "Power", "Battery, screen timeout, suspend behaviour."),
    ("region-language", "Region & Language", "Locale, formats, input methods."),
    ("sound", "Sound", "Output device, input device, volume profiles."),
    ("startup-apps", "Startup Apps", "What launches at login."),
    ("system", "System & Accounts", "User accounts, password, root."),
    ("time", "Time & Language", "Locale + clock combined."),
    ("touchpad", "Touchpad", "Tap to click, gestures, palm rejection."),
    ("users", "Users", "Add / remove / edit local users."),
    ("vpn", "VPN", "VPN profiles."),
    ("wallpaper", "Wallpaper", "Pick wallpaper, slideshow rotation."),
    ("window-management", "Window Management", "Tiling, focus follows mouse."),
    ("wired", "Wired", "Ethernet connection details."),
    ("wireless", "Wi-Fi", "Wi-Fi networks, security."),
    ("workspaces", "Workspaces", "Workspace policy + count."),
];

// ---------------------------------------------------------------------------
// settings.list_pages
// ---------------------------------------------------------------------------

struct ListPagesTool;

#[async_trait]
impl Tool for ListPagesTool {
    fn name(&self) -> &'static str {
        "settings.list_pages"
    }
    fn description(&self) -> &'static str {
        "List every settings page this build exposes. Each item has \
         id (the CLI subcommand), label (human-readable title), and \
         hint (one-line description of what's there)."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "additionalProperties": false })
    }
    async fn exec(&self, _input: Value) -> ToolResult {
        let pages: Vec<Value> = PAGES
            .iter()
            .map(|(id, label, hint)| {
                json!({ "id": id, "label": label, "hint": hint })
            })
            .collect();
        ToolResult::ok(json!({ "pages": pages }).to_string())
    }
}

// ---------------------------------------------------------------------------
// settings.search
// ---------------------------------------------------------------------------

struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "settings.search"
    }
    fn description(&self) -> &'static str {
        "Case-insensitive substring search over the page list. Pass \
         a natural-language fragment ('dark mode', 'audio output') \
         and get back the best-matching page ids."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 5}
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_lowercase(),
            None => return ToolResult::err("missing query"),
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let mut hits: Vec<Value> = PAGES
            .iter()
            .filter_map(|(id, label, hint)| {
                let hay = format!("{} {} {}", id, label.to_lowercase(), hint.to_lowercase());
                if hay.contains(&query) {
                    Some(json!({ "id": id, "label": label, "hint": hint }))
                } else {
                    None
                }
            })
            .collect();
        hits.truncate(limit);
        ToolResult::ok(json!({ "hits": hits }).to_string())
    }
}

// ---------------------------------------------------------------------------
// settings.open
// ---------------------------------------------------------------------------

struct OpenTool;

#[async_trait]
impl Tool for OpenTool {
    fn name(&self) -> &'static str {
        "settings.open"
    }
    fn description(&self) -> &'static str {
        "Launch the settings GUI, optionally jumping straight to a \
         specific page. Use this to hand control to the user; for \
         changes the agent can apply directly, prefer the kernel's \
         `cos config set` / `cos profile *` commands."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "page": {
                    "type": "string",
                    "description": "Page id, e.g. 'wireless' or 'appearance'. Omit for the default landing page."
                }
            },
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let page = input
            .get("page")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let argv: Vec<String> = match page {
            Some(p) => vec!["cosmic-settings".into(), p],
            None => vec!["cosmic-settings".into()],
        };
        let res = tokio::task::spawn_blocking(move || {
            let argv_b: Vec<&str> = argv.iter().map(String::as_str).collect();
            cos_runtime::exec::start(&argv_b)
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::ok(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::err(format!("settings.open: {e}")),
            Err(e) => ToolResult::err(format!("settings.open join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-settings", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(ListPagesTool))
            .tool(Arc::new(SearchTool))
            .tool(Arc::new(OpenTool))
            .serve_stdio()
            .await
            .map_err(|e| anyhow::anyhow!("cosmic-settings MCP server exited: {e}"))
    })
}

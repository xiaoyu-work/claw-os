//! MCP tool surface for `cosmic-settings`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening the settings GUI. The authoritative tool
//! descriptions and argument schemas live in `apps/cosmic-settings/app.json`.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};

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
    (
        "accessibility",
        "Accessibility",
        "Magnifier, screen reader, contrast.",
    ),
    ("about", "About", "OS version, hardware info, hostname."),
    ("agent", "Agent", "Default LLM provider, memory, approvals."),
    (
        "appearance",
        "Appearance",
        "Light/dark mode, accent colour, theme import/export.",
    ),
    ("applications", "Applications", "Installed app preferences."),
    (
        "bluetooth",
        "Bluetooth",
        "Pair, unpair, manage Bluetooth devices.",
    ),
    ("date-time", "Date & Time", "Timezone, clock format, NTP."),
    (
        "default-apps",
        "Default Apps",
        "Default browser / mail / image viewer / ...",
    ),
    (
        "desktop",
        "Desktop",
        "Wallpaper-less desktop, hot corners, animations.",
    ),
    (
        "displays",
        "Displays",
        "Resolution, refresh rate, scaling, night light.",
    ),
    ("dock", "Dock", "Position, autohide, size."),
    ("input", "Input", "Keyboard, mouse, touchpad master switch."),
    ("keyboard", "Keyboard", "Layouts, repeat rate, shortcuts."),
    (
        "legacy-applications",
        "Legacy Applications",
        "X11 application compatibility.",
    ),
    (
        "mouse",
        "Mouse",
        "Pointer speed, acceleration, scroll direction.",
    ),
    ("network", "Network", "Connections overview."),
    ("panel", "Panel", "Top bar size, position, applets."),
    (
        "power",
        "Power",
        "Battery, screen timeout, suspend behaviour.",
    ),
    (
        "region-language",
        "Region & Language",
        "Locale, formats, input methods.",
    ),
    (
        "sound",
        "Sound",
        "Output device, input device, volume profiles.",
    ),
    ("startup-apps", "Startup Apps", "What launches at login."),
    (
        "system",
        "System & Accounts",
        "User accounts, password, root.",
    ),
    ("time", "Time & Language", "Locale + clock combined."),
    (
        "touchpad",
        "Touchpad",
        "Tap to click, gestures, palm rejection.",
    ),
    ("users", "Users", "Add / remove / edit local users."),
    ("vpn", "VPN", "VPN profiles."),
    (
        "wallpaper",
        "Wallpaper",
        "Pick wallpaper, slideshow rotation.",
    ),
    (
        "window-management",
        "Window Management",
        "Tiling, focus follows mouse.",
    ),
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
    async fn handle(&self, _input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let pages: Vec<Value> = PAGES
            .iter()
            .map(|(id, label, hint)| json!({ "id": id, "label": label, "hint": hint }))
            .collect();
        ToolResult::text(json!({ "pages": pages }).to_string())
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
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_lowercase(),
            None => return ToolResult::error("missing query"),
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
        ToolResult::text(json!({ "hits": hits }).to_string())
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
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
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
            Ok(Ok(_)) => ToolResult::text(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::error(format!("settings.open: {e}")),
            Err(e) => ToolResult::error(format!("settings.open join: {e}")),
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
        let mut app = App::from_environment()?;
        app.bind(Arc::new(ListPagesTool))?;
        app.bind(Arc::new(SearchTool))?;
        app.bind(Arc::new(OpenTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("cosmic-settings MCP server exited: {error}"))
}

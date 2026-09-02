//! MCP tool surface for `cosmic-player` when launched in
//! `COS_MCP_SERVER=1` mode.
//!
//! The MCP server here does **not** start gstreamer or libcosmic. It
//! acts as an MPRIS2 D-Bus client and forwards control commands to
//! whichever media player is already on the session bus (typically a
//! live `cosmic-player` instance, but any MPRIS-compliant player
//! works — VLC, Rhythmbox, Spotify, …).
//!
//! If no MPRIS player is running, the tools return an error rather
//! than auto-launching one — the agent can decide whether to spawn
//! the player itself via `cos_app_run` first.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{Server, Tool, ToolResult};
use serde_json::{Value, json};
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::names::OwnedBusName;

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MediaPlayer2Player {
    fn play_pause(&self) -> zbus::Result<()>;
    fn play(&self) -> zbus::Result<()>;
    fn pause(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;
}

/// Find the first `org.mpris.MediaPlayer2.*` service on the session
/// bus. Returns the bus name (e.g. `org.mpris.MediaPlayer2.cosmic`).
async fn find_player(conn: &Connection) -> Result<String, String> {
    let dbus = DBusProxy::new(conn)
        .await
        .map_err(|e| format!("DBusProxy: {e}"))?;
    let names: Vec<OwnedBusName> = dbus
        .list_names()
        .await
        .map_err(|e| format!("ListNames: {e}"))?;
    for n in names {
        let s = n.as_str();
        if s.starts_with("org.mpris.MediaPlayer2.") {
            return Ok(s.to_string());
        }
    }
    Err("no MPRIS player on the session bus".to_string())
}

async fn with_player<F, Fut, T>(action: F) -> ToolResult
where
    F: FnOnce(MediaPlayer2PlayerProxy<'static>) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
    T: serde::Serialize,
{
    let conn = match Connection::session().await {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("session bus: {e}")),
    };
    let name = match find_player(&conn).await {
        Ok(n) => n,
        Err(e) => return ToolResult::err(e),
    };
    let builder = match MediaPlayer2PlayerProxy::builder(&conn).destination(name) {
        Ok(b) => b,
        Err(e) => return ToolResult::err(format!("proxy destination: {e}")),
    };
    let proxy = match builder.build().await {
        Ok(p) => p,
        Err(e) => return ToolResult::err(format!("proxy build: {e}")),
    };
    match action(proxy).await {
        Ok(v) => match serde_json::to_string(&v) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("encode: {e}")),
        },
        Err(e) => ToolResult::err(e),
    }
}

macro_rules! mpris_action_tool {
    ($struct_name:ident, $tool_name:literal, $desc:literal, $method:ident) => {
        struct $struct_name;
        #[async_trait]
        impl Tool for $struct_name {
            fn name(&self) -> &'static str {
                $tool_name
            }
            fn description(&self) -> &'static str {
                $desc
            }
            fn input_schema(&self) -> Value {
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })
            }
            async fn exec(&self, _input: Value) -> ToolResult {
                with_player(|p| async move {
                    p.$method().await.map_err(|e| format!("{}: {e}", $tool_name))?;
                    Ok(json!({"ok": true}))
                })
                .await
            }
        }
    };
}

mpris_action_tool!(PlayTool, "player.play", "Resume playback on the active MPRIS player.", play);
mpris_action_tool!(PauseTool, "player.pause", "Pause the active MPRIS player.", pause);
mpris_action_tool!(StopTool, "player.stop", "Stop playback on the active MPRIS player.", stop);
mpris_action_tool!(NextTool, "player.next", "Skip to the next track.", next);
mpris_action_tool!(PrevTool, "player.previous", "Skip to the previous track.", previous);
mpris_action_tool!(TogglePlayPauseTool, "player.toggle", "Toggle play/pause.", play_pause);

struct StatusTool;

#[async_trait]
impl Tool for StatusTool {
    fn name(&self) -> &'static str {
        "player.status"
    }
    fn description(&self) -> &'static str {
        "Return the current playback status (Playing/Paused/Stopped) and \
         metadata (title, artist, album, length-µs) of the active MPRIS player."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    async fn exec(&self, _input: Value) -> ToolResult {
        with_player(|p| async move {
            let status = p.playback_status().await.map_err(|e| format!("playback_status: {e}"))?;
            let meta = p.metadata().await.map_err(|e| format!("metadata: {e}"))?;
            // Pick a few standard MPRIS keys; downcast best-effort.
            let pick = |k: &str| -> Option<String> {
                meta.get(k)
                    .and_then(|v| <String>::try_from(v.clone()).ok())
            };
            let pick_arr = |k: &str| -> Vec<String> {
                meta.get(k)
                    .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
                    .unwrap_or_default()
            };
            let length = meta
                .get("mpris:length")
                .and_then(|v| <i64>::try_from(v.clone()).ok());
            Ok(json!({
                "status": status,
                "title": pick("xesam:title"),
                "artist": pick_arr("xesam:artist"),
                "album": pick("xesam:album"),
                "length_micros": length,
                "url": pick("xesam:url"),
            }))
        })
        .await
    }
}

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-player", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(PlayTool))
            .tool(Arc::new(PauseTool))
            .tool(Arc::new(StopTool))
            .tool(Arc::new(NextTool))
            .tool(Arc::new(PrevTool))
            .tool(Arc::new(TogglePlayPauseTool))
            .tool(Arc::new(StatusTool))
            .serve_stdio()
            .await
    })?;
    Ok(())
}

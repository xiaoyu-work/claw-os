//! MCP tool surface for `cosmic-player` when launched in
//! `COS_MCP_SERVER=1` mode.
//!
//! The MCP server here does **not** start gstreamer or libcosmic. It
//! acts as an MPRIS2 D-Bus client and forwards control commands to
//! whichever media player is already on the session bus (typically a
//! live `cosmic-player` instance, but any MPRIS-compliant player
//! works — VLC, Rhythmbox, Spotify, …).
//!
//! If no MPRIS player is running, the tools return an error. Launching a
//! desktop process is a separate typed, capability-gated action.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};
use zbus::fdo::DBusProxy;
use zbus::names::OwnedBusName;
use zbus::Connection;

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
    fn metadata(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;
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
        Err(e) => return ToolResult::error(format!("session bus: {e}")),
    };
    let name = match find_player(&conn).await {
        Ok(n) => n,
        Err(e) => return ToolResult::error(e),
    };
    let builder = match MediaPlayer2PlayerProxy::builder(&conn).destination(name) {
        Ok(b) => b,
        Err(e) => return ToolResult::error(format!("proxy destination: {e}")),
    };
    let proxy = match builder.build().await {
        Ok(p) => p,
        Err(e) => return ToolResult::error(format!("proxy build: {e}")),
    };
    match action(proxy).await {
        Ok(v) => match serde_json::to_string(&v) {
            Ok(s) => ToolResult::text(s),
            Err(e) => ToolResult::error(format!("encode: {e}")),
        },
        Err(e) => ToolResult::error(e),
    }
}

macro_rules! mpris_action_tool {
    ($struct_name:ident, $tool_name:literal, $method:ident) => {
        struct $struct_name;
        #[async_trait]
        impl Tool for $struct_name {
            fn name(&self) -> &'static str {
                $tool_name
            }
            async fn handle(&self, _input: Value, context: CallContext) -> ToolResult {
                if let Err(error) = context.check_cancelled() {
                    return ToolResult::error(error.to_string());
                }
                with_player(|p| async move {
                    p.$method().await.map_err(|e| format!("{}: {e}", $tool_name))?;
                    Ok(json!({"ok": true}))
                })
                .await
            }
        }
    };
}

mpris_action_tool!(PlayTool, "player.play", play);
mpris_action_tool!(PauseTool, "player.pause", pause);
mpris_action_tool!(StopTool, "player.stop", stop);
mpris_action_tool!(NextTool, "player.next", next);
mpris_action_tool!(PrevTool, "player.previous", previous);
mpris_action_tool!(TogglePlayPauseTool, "player.toggle", play_pause);

struct StatusTool;

#[async_trait]
impl Tool for StatusTool {
    fn name(&self) -> &'static str {
        "player.status"
    }
    async fn handle(&self, _input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        with_player(|p| async move {
            let status = p
                .playback_status()
                .await
                .map_err(|e| format!("playback_status: {e}"))?;
            let meta = p.metadata().await.map_err(|e| format!("metadata: {e}"))?;
            // Pick a few standard MPRIS keys; downcast best-effort.
            let pick = |k: &str| -> Option<String> {
                meta.get(k).and_then(|v| <String>::try_from(v.clone()).ok())
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
        let mut app = App::from_environment()?;
        app.bind(Arc::new(PlayTool))?;
        app.bind(Arc::new(PauseTool))?;
        app.bind(Arc::new(StopTool))?;
        app.bind(Arc::new(NextTool))?;
        app.bind(Arc::new(PrevTool))?;
        app.bind(Arc::new(TogglePlayPauseTool))?;
        app.bind(Arc::new(StatusTool))?;
        app.serve_stdio().await
    })?;
    Ok(())
}

use std::time::{Duration, Instant};

use claw_display_control::{desktop::Command, Epoch};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::display_session::registry::{GuiControl, Registry};

use super::Authorized;

const CALL_TIMEOUT: Duration = Duration::from_secs(2);
const READBACK_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Geometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
    id: u64,
    app_id: String,
    title: String,
    states: Vec<String>,
    output_count: usize,
    workspace_count: usize,
    geometry: Vec<Geometry>,
    manageable: bool,
}

impl Window {
    fn active(&self) -> bool {
        self.states.iter().any(|state| state == "activated")
    }

    fn value(&self, epoch: Epoch) -> Value {
        json!({
            "identifier": format!("{}:{}", epoch.label(), self.id),
            "app_id": self.app_id,
            "title": self.title,
            "states": self.states,
            "output_count": self.output_count,
            "workspace_count": self.workspace_count,
            "geometry": self.geometry,
            "manageable": self.manageable,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    ok: bool,
    error: String,
    windows: Vec<Window>,
    count: usize,
    requested: usize,
    before: Option<Window>,
}

pub(super) fn target(owner: u32, pid: u32) -> Result<GuiControl, String> {
    Registry::get()
        .and_then(|registry| registry.owner_control(owner, pid))
        .map_err(|error| format!("authenticated desktop target: {error}"))
}

pub(super) fn parse_identifier(value: &str) -> Result<(Epoch, u64), String> {
    let invalid = || "window identifier must contain a display epoch and native ID".to_string();
    let (label, number) = value.split_once(':').ok_or_else(invalid)?;
    if label.len() != 32
        || !label
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(invalid());
    }
    let mut epoch = [0; 16];
    hex::decode_to_slice(label, &mut epoch).map_err(|_| invalid())?;
    let id = number.parse::<u64>().map_err(|_| invalid())?;
    if id == 0 || number != id.to_string() {
        return Err(invalid());
    }
    Ok((Epoch(epoch), id))
}

fn bound_identifier(value: &str, epoch: Epoch) -> Result<u64, String> {
    let (bound, id) = parse_identifier(value)?;
    if bound != epoch {
        return Err("window identifier belongs to a different display epoch".into());
    }
    Ok(id)
}

async fn snapshot(control: &GuiControl, command: Command) -> Result<Snapshot, String> {
    let control = control.clone();
    let value = tokio::task::spawn_blocking(move || {
        control.desktop(command, Instant::now() + CALL_TIMEOUT)
    })
    .await
    .map_err(|error| format!("native desktop exchange task: {error}"))?
    .map_err(|error| format!("native desktop exchange: {error}"))?;
    let snapshot: Snapshot = serde_json::from_value(value)
        .map_err(|error| format!("native desktop response: {error}"))?;
    if !snapshot.ok {
        return Err(snapshot.error);
    }
    Ok(snapshot)
}

fn values(windows: &[Window], epoch: Epoch) -> Vec<Value> {
    let mut windows = windows.iter().collect::<Vec<_>>();
    windows.sort_by(|left, right| {
        right
            .active()
            .cmp(&left.active())
            .then_with(|| left.app_id.cmp(&right.app_id))
            .then_with(|| left.title.cmp(&right.title))
    });
    windows
        .into_iter()
        .map(|window| window.value(epoch))
        .collect()
}

pub(super) async fn request(
    control: &GuiControl,
    action: &str,
    identifier: Option<&str>,
    app_id: Option<&str>,
    _authorized: &Authorized,
) -> Result<Value, String> {
    let id = identifier
        .map(|value| bound_identifier(value, control.epoch))
        .transpose()?;
    let command = match action {
        "list" => Command::List,
        "focus" => Command::Focus {
            identifier: id.ok_or("focus requires a window identifier")?,
        },
        "close" => Command::Close {
            identifier: id.ok_or("close requires a window identifier")?,
        },
        "close-app" => Command::CloseApp {
            identifier: id.ok_or("close-app requires a window identifier")?,
            app_id: app_id.ok_or("close-app requires an AppID")?.to_string(),
        },
        _ => return Err("unsupported native desktop action".into()),
    };
    let mut state = snapshot(control, command).await?;
    if action == "list" {
        return Ok(json!({
            "windows": values(&state.windows, control.epoch),
            "count": state.count,
            "capabilities": ["close", "activate"],
        }));
    }
    let before = state
        .before
        .take()
        .ok_or("native desktop effect omitted its selected window")?;
    let requested = state.requested;
    let wait = match action {
        "focus" => Duration::from_millis(300),
        "close" => Duration::from_millis(500),
        "close-app" => Duration::from_millis(1200),
        _ => unreachable!("validated native desktop action"),
    };
    let deadline = Instant::now() + wait;
    loop {
        let observed = match action {
            "focus" => state
                .windows
                .iter()
                .any(|window| Some(window.id) == id && window.active()),
            "close" => !state.windows.iter().any(|window| Some(window.id) == id),
            "close-app" => !state
                .windows
                .iter()
                .any(|window| window.app_id == before.app_id),
            _ => unreachable!("validated native desktop action"),
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if observed || left.is_zero() {
            break;
        }
        tokio::time::sleep(READBACK_INTERVAL.min(left)).await;
        state = snapshot(control, Command::List).await?;
    }
    if action == "close-app" {
        let remaining = state
            .windows
            .into_iter()
            .filter(|window| window.app_id == before.app_id)
            .collect::<Vec<_>>();
        return Ok(json!({
            "action": action,
            "identifier": identifier,
            "app_id": before.app_id,
            "action_applied": true,
            "requested": requested,
            "remaining": values(&remaining, control.epoch),
            "remaining_count": remaining.len(),
        }));
    }
    let after = state.windows.iter().find(|window| Some(window.id) == id);
    let mut result = json!({
        "action": action,
        "identifier": identifier,
        "action_applied": true,
        "before": before.value(control.epoch),
        "after": after.map(|window| window.value(control.epoch)),
    });
    if action == "focus" {
        result["activated"] = json!(after.is_some_and(Window::active));
    } else {
        result["closed"] = json!(after.is_none());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/desktop/windows.rs"
    ));
}

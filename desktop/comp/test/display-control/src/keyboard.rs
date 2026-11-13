// SPDX-License-Identifier: GPL-3.0-only

use super::{ClientState, State};
use crate::display_authority::ClientOrigin;
use smithay::{
    backend::input::{ButtonState, KeyState},
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{ButtonEvent, MotionEvent},
    },
    utils::SERIAL_COUNTER,
};
use std::{
    collections::VecDeque,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
use wayland_server::Resource;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const SCHEMA: &str = "claw.private-keyboard/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stroke {
    code: u32,
    control: bool,
}

struct Request {
    sequence: u64,
    instance: String,
    surface: String,
    expires: u64,
    strokes: VecDeque<Stroke>,
    pointer: Option<PointerAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerAction {
    Panel,
    Primary(u16, u16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PointerStep {
    Enter((f64, f64)),
    Button(ButtonState),
    Leave((f64, f64)),
}

fn panel_click_steps(center: (f64, f64)) -> Result<[PointerStep; 4]> {
    let (width, height) = (center.0 * 2.0, center.1 * 2.0);
    if !(16.0..=512.0).contains(&width) || !(16.0..=128.0).contains(&height) {
        return Err("pointer gesture has no bounded acknowledged panel geometry".into());
    }
    Ok([
        PointerStep::Enter(center),
        PointerStep::Button(ButtonState::Pressed),
        PointerStep::Button(ButtonState::Released),
        PointerStep::Leave((width + 1.0, center.1)),
    ])
}

fn primary_click_steps(x: u16, y: u16) -> Result<[PointerStep; 3]> {
    if x >= 800 || y >= 600 {
        return Err("pointer gesture is outside the acknowledged primary geometry".into());
    }
    Ok([
        PointerStep::Enter((f64::from(x), f64::from(y))),
        PointerStep::Button(ButtonState::Pressed),
        PointerStep::Button(ButtonState::Released),
    ])
}

fn text_code(byte: u8) -> Option<u32> {
    const LETTERS: [u32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    match byte {
        b'a'..=b'z' => Some(LETTERS[usize::from(byte - b'a')]),
        b'1'..=b'9' => Some(u32::from(byte - b'1') + 2),
        b'0' => Some(11),
        b'-' => Some(12),
        b'.' => Some(52),
        b'/' => Some(53),
        b' ' => Some(57),
        b'\n' => Some(28),
        _ => None,
    }
}

fn decode(value: &serde_json::Value, previous: u64, now: u64) -> Result<Request> {
    let fields = value
        .as_object()
        .ok_or("keyboard request is not an object")?;
    let names = [
        "schema",
        "sequence",
        "instance",
        "surface",
        "expires_ms",
        "kind",
        "value",
    ];
    if fields.len() != names.len()
        || fields.keys().any(|key| !names.contains(&key.as_str()))
        || value["schema"] != SCHEMA
    {
        return Err("unsupported keyboard request shape".into());
    }
    let sequence = value["sequence"].as_u64().ok_or("keyboard sequence")?;
    let expires = value["expires_ms"].as_u64().ok_or("keyboard deadline")?;
    let instance = value["instance"].as_str().ok_or("keyboard instance")?;
    let surface = value["surface"].as_str().ok_or("keyboard surface")?;
    if sequence != previous + 1
        || sequence > 32
        || expires <= now
        || expires > now.saturating_add(5_000)
        || instance.len() != 32
        || !instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !matches!(surface, "primary" | "auxiliary" | "panel")
    {
        return Err("stale, replayed or unbound keyboard request".into());
    }
    let text = value["value"].as_str().ok_or("keyboard value")?;
    let pointer = match (value["kind"].as_str(), surface, text) {
        (Some("pointer"), "panel", "center-click") => Some(PointerAction::Panel),
        (Some("pointer"), "primary", text) => {
            let (x, y) = text.split_once(',').ok_or("primary pointer coordinates")?;
            let (x, y) = (x.parse::<u16>()?, y.parse::<u16>()?);
            primary_click_steps(x, y)?;
            if text != format!("{x},{y}") {
                return Err("noncanonical primary pointer coordinates".into());
            }
            Some(PointerAction::Primary(x, y))
        }
        (Some("pointer"), _, _) | (_, "panel", _) => {
            return Err("pointer/keyboard action does not match its bounded surface role".into());
        }
        _ => None,
    };
    let strokes = match value["kind"].as_str() {
        Some("pointer") => VecDeque::new(),
        Some("text") if !text.is_empty() && text.len() <= 128 => text
            .bytes()
            .map(|byte| {
                text_code(byte)
                    .map(|code| Stroke {
                        code,
                        control: false,
                    })
                    .ok_or("unsupported keyboard text")
            })
            .collect::<std::result::Result<VecDeque<_>, _>>()?,
        Some("key") => {
            let stroke = match text {
                "focus" => None,
                "ctrl-a" => Some(Stroke {
                    code: 30,
                    control: true,
                }),
                "ctrl-t" => Some(Stroke {
                    code: 20,
                    control: true,
                }),
                "ctrl-s" => Some(Stroke {
                    code: 31,
                    control: true,
                }),
                "enter" => Some(Stroke {
                    code: 28,
                    control: false,
                }),
                "escape" => Some(Stroke {
                    code: 1,
                    control: false,
                }),
                _ => return Err("unsupported keyboard chord".into()),
            };
            stroke.into_iter().collect()
        }
        _ => return Err("unsupported keyboard action".into()),
    };
    Ok(Request {
        sequence,
        instance: instance.into(),
        surface: surface.into(),
        expires,
        strokes,
        pointer,
    })
}

fn pointer_click(state: &mut State, instance: &str, action: PointerAction, now: u64) -> Result<()> {
    let window = state
        .native_window
        .as_ref()
        .ok_or("native window is absent")?;
    let (surface, steps) = match action {
        PointerAction::Panel => {
            let (surface, center) = window
                .panel_pointer_target(instance)
                .ok_or("pointer target is not this live configured buffered panel")?;
            (surface, panel_click_steps(center)?.to_vec())
        }
        PointerAction::Primary(x, y) => (
            window
                .primary_pointer_target(instance)
                .ok_or("pointer target is not this live configured 800x600 primary surface")?,
            primary_click_steps(x, y)?.to_vec(),
        ),
    };
    let client = state.common.display_handle.get_client(surface.id())?;
    let origin = client
        .get_data::<ClientState>()
        .and_then(|data| data.display_origin.as_ref())
        .ok_or("pointer target has no authenticated display origin")?;
    if !matches!(origin, ClientOrigin::Instance(_))
        || (action == PointerAction::Panel && !origin.layer_shell())
        || origin.selection_read()
        || origin.selection_write()
        || origin
            .instance()
            .map(|id| claw_display_control::Epoch(id.0).label())
            .as_deref()
            != Some(instance)
    {
        return Err("pointer target lost its live Root instance or required panel lease".into());
    }
    let pointer = state
        .seat
        .get_pointer()
        .ok_or("fixture pointer unavailable")?;
    if pointer.is_grabbed() {
        return Err("private pointer already has an unrelated grab".into());
    }
    for step in steps {
        match step {
            PointerStep::Enter(location) => pointer.motion(
                state,
                Some((surface.clone(), (0.0, 0.0).into())),
                &MotionEvent {
                    location: location.into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: now as u32,
                },
            ),
            PointerStep::Button(button_state) => pointer.button(
                state,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: now as u32,
                    button: 0x110,
                    state: button_state,
                },
            ),
            // End hover too: the original tooltip's queued task checks
            // CursorLeft's shared hover state, not the later has_popup flag.
            PointerStep::Leave(location) => pointer.motion(
                state,
                None,
                &MotionEvent {
                    location: location.into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: now as u32,
                },
            ),
        }
    }
    pointer.frame(state);
    Ok(())
}

fn acknowledge(state: &State, directory: &Path, request: &Request) -> Result<()> {
    let mut ack = serde_json::json!({
        "schema": SCHEMA, "sequence": request.sequence,
        "instance": request.instance, "surface": request.surface,
        "buffer_commits_at_delivery": state.native_window.as_ref()
            .and_then(|window| window.input_buffer_commit(&request.instance, &request.surface))
            .ok_or("input target lost its live buffer before acknowledgement")?,
    });
    ack[if request.pointer.is_some() {
        "pointer_events_sent"
    } else {
        "keyboard_events_sent"
    }] = true.into();
    let temporary = directory.join("keyboard-ack.next");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(&ack)?)?;
    drop(file);
    std::fs::rename(temporary, directory.join("keyboard-ack.json"))?;
    Ok(())
}

fn read_request(path: &Path) -> Result<Option<serde_json::Value>> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 4096
    {
        return Err("keyboard request is not protected fixture state".into());
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err("keyboard request grew beyond its bound".into());
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

#[derive(Default)]
pub(super) struct Control {
    sequence: u64,
    active: Option<Request>,
}

impl Control {
    pub(super) fn step(&mut self, state: &mut State) -> Result<()> {
        if state.native_window.is_none() {
            return Ok(());
        }
        let directory = Path::new(crate::native_window::OBSERVER);
        let now = claw_display_control::wire::monotonic_ms()?;
        if self.active.is_none() {
            let Some(value) = read_request(&directory.join("keyboard-command.json"))? else {
                return Ok(());
            };
            if self.sequence > 0 && value["sequence"].as_u64() == Some(self.sequence) {
                return Ok(());
            }
            let request = decode(&value, self.sequence, now)?;
            self.sequence = request.sequence;
            self.active = Some(request);
        }
        let request = self.active.as_mut().unwrap();
        if now >= request.expires {
            return Err("keyboard command expired before delivery".into());
        }
        if let Some(action) = request.pointer {
            pointer_click(state, &request.instance, action, now)?;
            acknowledge(state, directory, request)?;
            self.active = None;
            return Ok(());
        }
        let surface = state
            .native_window
            .as_ref()
            .unwrap()
            .input_surface(&request.instance, &request.surface)
            .ok_or("keyboard target is not a live buffered selected surface")?;
        let client = state.common.display_handle.get_client(surface.id())?;
        let origin = client
            .get_data::<ClientState>()
            .and_then(|data| data.display_origin.as_ref())
            .ok_or("keyboard target has no authenticated display origin")?;
        if !matches!(origin, ClientOrigin::Instance(_))
            || origin
                .instance()
                .map(|id| claw_display_control::Epoch(id.0).label())
                .as_deref()
                != Some(request.instance.as_str())
        {
            return Err("keyboard target is not this authenticated instance".into());
        }
        let keyboard = state
            .seat
            .get_keyboard()
            .ok_or("fixture keyboard unavailable")?;
        if !keyboard.pressed_keys().is_empty() {
            return Err("fixture keyboard already has held keys".into());
        }
        keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
        if let Some(stroke) = request.strokes.pop_front() {
            let mut keys = Vec::with_capacity(2);
            if stroke.control {
                keys.push(29);
            }
            keys.push(stroke.code);
            for code in keys.iter().copied() {
                keyboard.input::<(), _>(
                    state,
                    Keycode::from(code + 8),
                    KeyState::Pressed,
                    SERIAL_COUNTER.next_serial(),
                    now as u32,
                    |_, _, _| FilterResult::Forward,
                );
            }
            for code in keys.into_iter().rev() {
                keyboard.input::<(), _>(
                    state,
                    Keycode::from(code + 8),
                    KeyState::Released,
                    SERIAL_COUNTER.next_serial(),
                    now as u32,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
        if request.strokes.is_empty() {
            acknowledge(state, directory, request)?;
            self.active = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> serde_json::Value {
        serde_json::json!({
            "schema": SCHEMA, "sequence": 1, "instance": "0123456789abcdef0123456789abcdef",
            "surface": "primary", "expires_ms": 2000, "kind": "text", "value": "a1-. \n"
        })
    }

    #[test]
    fn keyboard_plan_refuses_replay_expiry_unknown_fields_and_unpaired_text() {
        let value = request();
        assert_eq!(decode(&value, 0, 1000).unwrap().strokes.len(), 6);
        assert!(decode(&value, 1, 1000).is_err());
        assert!(decode(&value, 0, 2000).is_err());
        let mut bad = value.clone();
        bad["approved"] = true.into();
        assert!(decode(&bad, 0, 1000).is_err());
        let mut bad = value.clone();
        bad["value"] = "a\tb".into();
        assert!(decode(&bad, 0, 1000).is_err());
        bad["value"] = "a".repeat(129).into();
        assert!(decode(&bad, 0, 1000).is_err());
        let mut bad = value;
        bad["surface"] = "foreign".into();
        assert!(decode(&bad, 0, 1000).is_err());
    }

    #[test]
    fn pointer_plan_refuses_wrong_roles_outside_geometry_and_replay() {
        let mut value = request();
        value["surface"] = "panel".into();
        assert!(decode(&value, 0, 1000).is_err());
        value["kind"] = "key".into();
        value["value"] = "focus".into();
        assert!(decode(&value, 0, 1000).is_err());
        value["kind"] = "pointer".into();
        value["value"] = "center-click".into();
        assert_eq!(
            decode(&value, 0, 1000).unwrap().pointer,
            Some(PointerAction::Panel)
        );
        assert!(decode(&value, 1, 1000).is_err());
        assert!(decode(&value, 0, 2000).is_err());
        for surface in ["primary", "auxiliary", "foreign"] {
            let mut bad = value.clone();
            bad["surface"] = surface.into();
            assert!(decode(&bad, 0, 1000).is_err());
        }
        let mut bad = value.clone();
        bad["expires_ms"] = 6001.into();
        assert!(decode(&bad, 0, 1000).is_err());
        let mut bad = value.clone();
        bad["x"] = 12.into();
        assert!(decode(&bad, 0, 1000).is_err());
        value["value"] = "12,20".into();
        assert!(decode(&value, 0, 1000).is_err());
        value["surface"] = "primary".into();
        assert_eq!(
            decode(&value, 0, 1000).unwrap().pointer,
            Some(PointerAction::Primary(12, 20))
        );
        for invalid in [
            "800,0",
            "0,600",
            "-1,1",
            "1,NaN",
            "01,1",
            "1,1,1",
            "center-click",
        ] {
            value["value"] = invalid.into();
            assert!(decode(&value, 0, 1000).is_err(), "{invalid}");
        }
    }

    #[test]
    fn pointer_plan_refuses_lingering_hover_or_held_button_at_delivery_end() {
        for center in [(24.0, 20.0), (8.0, 8.0), (256.0, 64.0)] {
            let (mut hovered, mut held) = (false, false);
            for step in panel_click_steps(center).unwrap() {
                match step {
                    PointerStep::Enter(location) => {
                        assert_eq!(location, center);
                        hovered = true;
                    }
                    PointerStep::Button(ButtonState::Pressed) => {
                        assert!(hovered && !held);
                        held = true;
                    }
                    PointerStep::Button(ButtonState::Released) => {
                        assert!(held);
                        held = false;
                    }
                    PointerStep::Leave(location) => {
                        assert!(!held);
                        assert!(location.0 > center.0 * 2.0);
                        hovered = false;
                    }
                }
            }
            assert!(
                !hovered && !held,
                "a queued tooltip must see hover cancelled"
            );
        }
        for center in [(0.0, 20.0), (257.0, 20.0), (24.0, 65.0), (f64::NAN, 20.0)] {
            assert!(panel_click_steps(center).is_err());
        }
        let (mut hovered, mut held) = (false, false);
        for step in primary_click_steps(500, 80).unwrap() {
            match step {
                PointerStep::Enter(_) => hovered = true,
                PointerStep::Button(ButtonState::Pressed) => {
                    assert!(hovered && !held);
                    held = true;
                }
                PointerStep::Button(ButtonState::Released) => {
                    assert!(held);
                    held = false;
                }
                PointerStep::Leave(_) => hovered = false,
            }
        }
        assert!(
            hovered && !held,
            "a primary click must not batch CursorLeft with the hit"
        );
    }
}

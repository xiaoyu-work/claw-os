use std::collections::BTreeSet;
use std::thread;
use std::time::Duration;

use cctk::cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use cctk::cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1;
use cctk::sctk;
use cctk::sctk::registry::{ProvidesRegistryState, RegistryState};
use cctk::sctk::seat::{Capability, SeatHandler, SeatState};
use cctk::toplevel_info::{ToplevelInfoHandler, ToplevelInfoState};
use cctk::toplevel_management::{ToplevelManagerHandler, ToplevelManagerState};
use cctk::wayland_client::globals::registry_queue_init;
use cctk::wayland_client::protocol::wl_seat::WlSeat;
use cctk::wayland_client::{Connection, QueueHandle, WEnum};
use cosmic_client_toolkit as cctk;
use serde_json::{json, Value};

struct DesktopState {
    registry_state: RegistryState,
    seat_state: SeatState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: ToplevelManagerState,
    info_generation: u64,
    capabilities: BTreeSet<u32>,
}

impl ProvidesRegistryState for DesktopState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!();
}

impl SeatHandler for DesktopState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl ToplevelInfoHandler for DesktopState {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &cctk::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }

    fn info_done(&mut self, _: &Connection, _: &QueueHandle<Self>) {
        self.info_generation = self.info_generation.saturating_add(1);
    }
}

impl ToplevelManagerHandler for DesktopState {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager_state
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        capabilities: Vec<
            WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>,
        >,
    ) {
        self.capabilities = capabilities
            .into_iter()
            .map(|capability| match capability {
                WEnum::Value(value) => value as u32,
                WEnum::Unknown(value) => value,
            })
            .collect();
    }
}

sctk::delegate_registry!(DesktopState);
sctk::delegate_seat!(DesktopState);
cctk::delegate_toplevel_info!(DesktopState);
cctk::delegate_toplevel_manager!(DesktopState);

pub fn helper(args: &[String]) -> Result<Value, String> {
    let action = args
        .first()
        .ok_or_else(|| "desktop helper action is required".to_string())?;
    let identifier = args.get(1).map(String::as_str);
    let expected_app_id = args.get(2).map(String::as_str);
    validate_helper_args(action, identifier, expected_app_id)?;

    let connection =
        Connection::connect_to_env().map_err(|error| format!("connect to Wayland: {error}"))?;
    let (globals, mut event_queue) = registry_queue_init(&connection)
        .map_err(|error| format!("initialize Wayland registry: {error}"))?;
    let qh = event_queue.handle();
    let registry_state = RegistryState::new(&globals);
    let toplevel_info_state = ToplevelInfoState::try_new(&registry_state, &qh)
        .ok_or_else(|| "compositor does not support ext-foreign-toplevel-list-v1".to_string())?;
    let toplevel_manager_state = ToplevelManagerState::try_new(&registry_state, &qh)
        .ok_or_else(|| "compositor does not support COSMIC toplevel management".to_string())?;
    let mut state = DesktopState {
        seat_state: SeatState::new(&globals, &qh),
        registry_state,
        toplevel_info_state,
        toplevel_manager_state,
        info_generation: 0,
        capabilities: BTreeSet::new(),
    };
    for _ in 0..4 {
        event_queue
            .roundtrip(&mut state)
            .map_err(|error| format!("read Wayland toplevels: {error}"))?;
        if state.info_generation > 0 && !state.capabilities.is_empty() {
            break;
        }
    }

    let before = windows(&state);
    match action.as_str() {
        "list" => {
            let count = before.len();
            Ok(json!({
                "windows": before,
                "count": count,
                "capabilities": capability_names(&state),
            }))
        }
        "focus" => {
            require_capability(
                &state,
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Activate,
                "activate",
            )?;
            let window = selected_window(&state, identifier.unwrap(), expected_app_id)?;
            let cosmic = window
                .cosmic_toplevel
                .clone()
                .ok_or_else(|| "selected window has no COSMIC management handle".to_string())?;
            let seats = state.seat_state.seats().collect::<Vec<_>>();
            if seats.is_empty() {
                return Err("Wayland compositor reported no seat for activation".to_string());
            }
            for seat in seats {
                state
                    .toplevel_manager_state
                    .manager
                    .activate(&cosmic, &seat);
            }
            refresh(&mut event_queue, &mut state, 3)?;
            let after = selected_window_value(&state, identifier.unwrap());
            let activated = after
                .as_ref()
                .and_then(|value| value["states"].as_array())
                .is_some_and(|states| states.iter().any(|state| state == "activated"));
            Ok(json!({
                "action": "focus",
                "identifier": identifier,
                "action_applied": true,
                "before": window_value(&window),
                "after": after,
                "activated": activated,
            }))
        }
        "close" => {
            require_capability(
                &state,
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Close,
                "close",
            )?;
            let window = selected_window(&state, identifier.unwrap(), expected_app_id)?;
            let cosmic = window
                .cosmic_toplevel
                .clone()
                .ok_or_else(|| "selected window has no COSMIC management handle".to_string())?;
            state.toplevel_manager_state.manager.close(&cosmic);
            refresh(&mut event_queue, &mut state, 5)?;
            let after = selected_window_value(&state, identifier.unwrap());
            let closed = after.is_none();
            Ok(json!({
                "action": "close",
                "identifier": identifier,
                "action_applied": true,
                "before": window_value(&window),
                "after": after,
                "closed": closed,
            }))
        }
        "close-app" => {
            require_capability(
                &state,
                zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Close,
                "close",
            )?;
            let selected =
                selected_window(&state, identifier.unwrap(), Some(expected_app_id.unwrap()))?;
            let app_id = selected.app_id.clone();
            let handles = state
                .toplevel_info_state
                .toplevels()
                .filter(|window| window.app_id == app_id)
                .filter_map(|window| window.cosmic_toplevel.clone())
                .collect::<Vec<_>>();
            if handles.is_empty() {
                return Err("application has no manageable COSMIC windows".to_string());
            }
            for handle in &handles {
                state.toplevel_manager_state.manager.close(handle);
            }
            refresh(&mut event_queue, &mut state, 12)?;
            let remaining = state
                .toplevel_info_state
                .toplevels()
                .filter(|window| window.app_id == app_id)
                .map(window_value)
                .collect::<Vec<_>>();
            let remaining_count = remaining.len();
            Ok(json!({
                "action": "close-app",
                "identifier": identifier,
                "app_id": app_id,
                "action_applied": true,
                "requested": handles.len(),
                "remaining": remaining,
                "remaining_count": remaining_count,
            }))
        }
        _ => unreachable!("validated desktop helper action"),
    }
}

fn refresh(
    event_queue: &mut cctk::wayland_client::EventQueue<DesktopState>,
    state: &mut DesktopState,
    attempts: usize,
) -> Result<(), String> {
    for _ in 0..attempts {
        event_queue
            .roundtrip(state)
            .map_err(|error| format!("refresh Wayland toplevels: {error}"))?;
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn selected_window(
    state: &DesktopState,
    identifier: &str,
    expected_app_id: Option<&str>,
) -> Result<cctk::toplevel_info::ToplevelInfo, String> {
    let window = state
        .toplevel_info_state
        .toplevels()
        .find(|window| window.identifier == identifier)
        .cloned()
        .ok_or_else(|| format!("window not found: {identifier}"))?;
    if let Some(expected_app_id) = expected_app_id {
        if window.app_id != expected_app_id {
            return Err(format!(
                "window app_id changed: expected {expected_app_id:?}, found {:?}",
                window.app_id
            ));
        }
    }
    Ok(window)
}

fn selected_window_value(state: &DesktopState, identifier: &str) -> Option<Value> {
    state
        .toplevel_info_state
        .toplevels()
        .find(|window| window.identifier == identifier)
        .map(window_value)
}

fn windows(state: &DesktopState) -> Vec<Value> {
    let mut windows = state
        .toplevel_info_state
        .toplevels()
        .map(window_value)
        .collect::<Vec<_>>();
    windows.sort_by(|left, right| {
        let left_active = left["states"]
            .as_array()
            .is_some_and(|states| states.iter().any(|state| state == "activated"));
        let right_active = right["states"]
            .as_array()
            .is_some_and(|states| states.iter().any(|state| state == "activated"));
        right_active
            .cmp(&left_active)
            .then_with(|| left["app_id"].as_str().cmp(&right["app_id"].as_str()))
            .then_with(|| left["title"].as_str().cmp(&right["title"].as_str()))
    });
    windows
}

fn window_value(window: &cctk::toplevel_info::ToplevelInfo) -> Value {
    let mut states = window
        .state
        .iter()
        .map(|state| match state {
            zcosmic_toplevel_handle_v1::State::Maximized => "maximized",
            zcosmic_toplevel_handle_v1::State::Minimized => "minimized",
            zcosmic_toplevel_handle_v1::State::Activated => "activated",
            zcosmic_toplevel_handle_v1::State::Fullscreen => "fullscreen",
            zcosmic_toplevel_handle_v1::State::Sticky => "sticky",
            _ => "unknown",
        })
        .collect::<Vec<_>>();
    states.sort_unstable();
    let geometry = window
        .geometry
        .values()
        .map(|geometry| {
            json!({
                "x": geometry.x,
                "y": geometry.y,
                "width": geometry.width,
                "height": geometry.height,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "identifier": window.identifier,
        "app_id": window.app_id,
        "title": window.title,
        "states": states,
        "output_count": window.output.len(),
        "workspace_count": window.workspace.len(),
        "geometry": geometry,
        "manageable": window.cosmic_toplevel.is_some(),
    })
}

fn capability_names(state: &DesktopState) -> Vec<&'static str> {
    [
        (
            zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Close,
            "close",
        ),
        (
            zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1::Activate,
            "activate",
        ),
    ]
    .into_iter()
    .filter_map(|(capability, name)| {
        state
            .capabilities
            .contains(&(capability as u32))
            .then_some(name)
    })
    .collect()
}

fn require_capability(
    state: &DesktopState,
    capability: zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1,
    name: &str,
) -> Result<(), String> {
    if !state.capabilities.contains(&(capability as u32)) {
        return Err(format!(
            "compositor does not advertise the {name} toplevel capability"
        ));
    }
    Ok(())
}

fn validate_helper_args(
    action: &str,
    identifier: Option<&str>,
    expected_app_id: Option<&str>,
) -> Result<(), String> {
    match action {
        "list" if identifier.is_none() && expected_app_id.is_none() => Ok(()),
        "focus" | "close" if valid_identifier(identifier) && expected_app_id.is_none() => Ok(()),
        "close-app" if valid_identifier(identifier) && valid_app_id(expected_app_id) => Ok(()),
        "list" => Err("list does not accept a window identifier".to_string()),
        "focus" | "close" => Err(format!("{action} requires one valid window identifier")),
        "close-app" => {
            Err("close-app requires a window identifier and expected app_id".to_string())
        }
        _ => Err(format!("unknown desktop helper action: {action}")),
    }
}

fn valid_identifier(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 512
            && !value.starts_with('-')
            && !value.chars().any(|character| character.is_control())
    })
}

fn valid_app_id(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 255
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
}

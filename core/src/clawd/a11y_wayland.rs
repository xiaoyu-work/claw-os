use cctk::cosmic_protocols::a11y::v1::client::cosmic_a11y_manager_v1;
use cctk::wayland_client::globals::{registry_queue_init, GlobalListContents};
use cctk::wayland_client::protocol::wl_registry;
use cctk::wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use cosmic_client_toolkit as cctk;
use serde_json::{json, Value};

#[derive(Default)]
struct A11yState {
    magnifier: Option<bool>,
    inverted: Option<bool>,
    filter: Option<String>,
    filter_active: Option<bool>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for A11yState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<cosmic_a11y_manager_v1::CosmicA11yManagerV1, ()> for A11yState {
    fn event(
        state: &mut Self,
        _: &cosmic_a11y_manager_v1::CosmicA11yManagerV1,
        event: cosmic_a11y_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            cosmic_a11y_manager_v1::Event::Magnifier { active } => {
                state.magnifier = Some(active_state(active));
            }
            cosmic_a11y_manager_v1::Event::ScreenFilter { inverted, filter } => {
                state.inverted = Some(active_state(inverted));
                state.filter = filter_name(filter);
                state.filter_active = Some(state.filter.is_some());
            }
            cosmic_a11y_manager_v1::Event::ScreenFilter2 {
                inverted,
                filter,
                filter_state,
            } => {
                state.inverted = Some(active_state(inverted));
                state.filter = filter_name(filter);
                state.filter_active = Some(active_state(filter_state));
            }
            _ => {}
        }
    }
}

pub fn helper(args: &[String]) -> Result<Value, String> {
    let action = args
        .first()
        .ok_or_else(|| "a11y helper action is required".to_string())?;
    let value = args.get(1).map(String::as_str);
    validate_args(action, value)?;
    let connection =
        Connection::connect_to_env().map_err(|error| format!("connect to Wayland: {error}"))?;
    let (globals, mut queue) = registry_queue_init(&connection)
        .map_err(|error| format!("initialize Wayland registry: {error}"))?;
    let qh = queue.handle();
    let manager = globals
        .bind::<cosmic_a11y_manager_v1::CosmicA11yManagerV1, _, _>(&qh, 1..=3, ())
        .map_err(|_| "compositor does not support cosmic_a11y_manager_v1".to_string())?;
    let mut state = A11yState::default();
    queue
        .roundtrip(&mut state)
        .map_err(|error| format!("read accessibility state: {error}"))?;
    let before = state_value(&state, manager.version());
    match action.as_str() {
        "status" => Ok(before),
        "magnifier" => {
            manager.set_magnifier(to_active(value.unwrap() == "on"));
            refresh(&mut queue, &mut state)?;
            Ok(change(
                "magnifier",
                before,
                state_value(&state, manager.version()),
            ))
        }
        "invert" => {
            if manager.version() < 2 {
                return Err(
                    "compositor accessibility protocol does not support screen filters".to_string(),
                );
            }
            set_filter(
                &manager,
                value.unwrap() == "on",
                state.filter.as_deref(),
                state.filter_active.unwrap_or(false),
            );
            refresh(&mut queue, &mut state)?;
            Ok(change(
                "invert",
                before,
                state_value(&state, manager.version()),
            ))
        }
        "filter" => {
            if manager.version() < 2 {
                return Err(
                    "compositor accessibility protocol does not support screen filters".to_string(),
                );
            }
            let filter = (value.unwrap() != "off").then_some(value.unwrap());
            set_filter(
                &manager,
                state.inverted.unwrap_or(false),
                filter,
                filter.is_some(),
            );
            refresh(&mut queue, &mut state)?;
            Ok(change(
                "filter",
                before,
                state_value(&state, manager.version()),
            ))
        }
        _ => unreachable!("validated a11y action"),
    }
}

fn refresh(
    queue: &mut cctk::wayland_client::EventQueue<A11yState>,
    state: &mut A11yState,
) -> Result<(), String> {
    for _ in 0..3 {
        queue
            .roundtrip(state)
            .map_err(|error| format!("refresh accessibility state: {error}"))?;
    }
    Ok(())
}

fn set_filter(
    manager: &cosmic_a11y_manager_v1::CosmicA11yManagerV1,
    inverted: bool,
    filter: Option<&str>,
    filter_active: bool,
) {
    let filter_value = to_filter(filter);
    if manager.version() >= 3 {
        manager.set_screen_filter2(to_active(inverted), filter_value, to_active(filter_active));
    } else {
        manager.set_screen_filter(
            to_active(inverted),
            match filter {
                None => cosmic_a11y_manager_v1::Filter::Disabled,
                Some(_) => filter_value,
            },
        );
    }
}

fn state_value(state: &A11yState, version: u32) -> Value {
    json!({
        "available": true,
        "protocol_version": version,
        "magnifier": state.magnifier,
        "inverted": state.inverted,
        "filter": state.filter,
        "filter_active": state.filter_active,
    })
}

fn change(action: &str, before: Value, after: Value) -> Value {
    json!({
        "action": action,
        "changed": before != after,
        "before": before,
        "after": after,
    })
}

fn active_state(value: WEnum<cosmic_a11y_manager_v1::ActiveState>) -> bool {
    matches!(
        value,
        WEnum::Value(cosmic_a11y_manager_v1::ActiveState::Enabled)
    )
}

fn to_active(value: bool) -> cosmic_a11y_manager_v1::ActiveState {
    if value {
        cosmic_a11y_manager_v1::ActiveState::Enabled
    } else {
        cosmic_a11y_manager_v1::ActiveState::Disabled
    }
}

fn filter_name(value: WEnum<cosmic_a11y_manager_v1::Filter>) -> Option<String> {
    match value {
        WEnum::Value(cosmic_a11y_manager_v1::Filter::Disabled) => None,
        WEnum::Value(cosmic_a11y_manager_v1::Filter::Greyscale) => Some("greyscale".to_string()),
        WEnum::Value(cosmic_a11y_manager_v1::Filter::DaltonizeProtanopia) => {
            Some("protanopia".to_string())
        }
        WEnum::Value(cosmic_a11y_manager_v1::Filter::DaltonizeDeuteranopia) => {
            Some("deuteranopia".to_string())
        }
        WEnum::Value(cosmic_a11y_manager_v1::Filter::DaltonizeTritanopia) => {
            Some("tritanopia".to_string())
        }
        WEnum::Value(cosmic_a11y_manager_v1::Filter::Unknown) | WEnum::Unknown(_) => {
            Some("unknown".to_string())
        }
        _ => Some("unknown".to_string()),
    }
}

fn to_filter(value: Option<&str>) -> cosmic_a11y_manager_v1::Filter {
    match value {
        None => cosmic_a11y_manager_v1::Filter::Unknown,
        Some("greyscale") => cosmic_a11y_manager_v1::Filter::Greyscale,
        Some("protanopia") => cosmic_a11y_manager_v1::Filter::DaltonizeProtanopia,
        Some("deuteranopia") => cosmic_a11y_manager_v1::Filter::DaltonizeDeuteranopia,
        Some("tritanopia") => cosmic_a11y_manager_v1::Filter::DaltonizeTritanopia,
        _ => cosmic_a11y_manager_v1::Filter::Unknown,
    }
}

fn validate_args(action: &str, value: Option<&str>) -> Result<(), String> {
    match action {
        "status" if value.is_none() => Ok(()),
        "magnifier" | "invert" if matches!(value, Some("on" | "off")) => Ok(()),
        "filter"
            if matches!(
                value,
                Some("off" | "greyscale" | "protanopia" | "deuteranopia" | "tritanopia")
            ) =>
        {
            Ok(())
        }
        "status" => Err("status does not accept a value".to_string()),
        "magnifier" | "invert" => Err(format!("{action} requires on|off")),
        "filter" => {
            Err("filter requires off|greyscale|protanopia|deuteranopia|tritanopia".to_string())
        }
        _ => Err(format!("unknown a11y helper action: {action}")),
    }
}

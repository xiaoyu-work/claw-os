use super::*;

#[cfg(target_os = "linux")]
#[test]
fn every_locale_category_is_presentation_not_panel_authority() {
    let keys: std::collections::BTreeSet<_> = PresentationKey::ALL
        .iter()
        .map(|key| key.as_str())
        .collect();
    assert_eq!(keys.len(), PresentationKey::ALL.len());
    for name in claw_display_control::locale::KEYS {
        let key = PresentationKey::ALL
            .iter()
            .find(|key| key.as_str() == *name)
            .unwrap();
        assert!(!key.is_panel(), "{name}");
    }
}

#[test]
fn display_bus_loader_and_authority_claims_are_not_presentation_keys() {
    for key in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "WAYLAND_SOCKET",
        "DBUS_SESSION_BUS_ADDRESS",
        "CLAW_DISPLAY_CONTROL_FD",
        "LD_PRELOAD",
        "HOME",
        "owner_uid",
        "clipboard.read",
    ] {
        assert!(
            serde_json::from_value::<Presentation>(serde_json::json!({(key): "claimed"})).is_err(),
            "{key}",
        );
    }
}

#[test]
fn gui_launch_does_not_accept_process_owner_or_lease_metadata() {
    for key in [
        "owner_uid",
        "pid",
        "cgroup",
        "epoch",
        "authority",
        "expires_monotonic_ms",
        "gui_entry",
        "entry",
        "selector",
        "runtime",
    ] {
        let value = serde_json::json!({
            "session_id": "app-fixture", "handle": "bound", "args": [], (key): "claimed",
        });
        assert!(
            serde_json::from_value::<LaunchRequest>(value).is_err(),
            "{key}"
        );
    }

}

#[test]
fn selected_read_only_accepts_bounded_native_nodes_and_opaque_status_references() {
    let request: FileReadRequest =
        serde_json::from_str(r#"{"session":"app-window","node":2.0e0}"#).unwrap();
    assert_eq!(request.node.get(), 2);
    for node in ["0", "1", "4097", "-2", "2.00000000000000000000001", "\"2\""] {
        assert!(serde_json::from_str::<FileReadRequest>(
            &format!(r#"{{"session":"app-window","node":{node}}}"#)).is_err(), "{node}");
    }
    for key in ["path", "owner_uid", "window", "pid", "selected", "grant", "access"] {
        assert!(serde_json::from_value::<FileReadRequest>(serde_json::json!({
            "session": "app-window", "node": 2, (key): "claimed",
        })).is_err(), "{key}");
    }
    let id = format!("sr-{}", "a".repeat(32));
    assert!(serde_json::from_value::<FileReadStatusRequest>(serde_json::json!({
        "session": "app-window", "selection_id": id, "node": 2,
    })).is_ok());
    assert!(serde_json::from_value::<FileReadStatusRequest>(serde_json::json!({
        "session": "app-window", "selection_id": "../foreign", "node": 2,
    })).is_err());
}

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

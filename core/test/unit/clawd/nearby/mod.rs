use super::*;

pub(super) fn fixture() -> NearbyView {
    NearbyView {
        schema: SCHEMA,
        owner_uid: 1000,
        probe: RadioProbe {
            available: true,
            mode: Some(RadioMode::Shared),
            driver: Some("fixture".into()),
            diagnostics: Vec::new(),
        },
        inbox: "/home/user/Downloads/Claw Nearby".into(),
        uploads: Vec::new(),
        session: Some(NearbySession {
            id: uuid::Uuid::new_v4().to_string(),
            phase: SessionPhase::Ready,
            mode: RadioMode::Shared,
            receive: true,
            expires_at: "2026-11-01T12:00:00.000Z".into(),
            peers: Vec::new(),
            offers: Vec::new(),
            transfers: Vec::new(),
            discovering: false,
            message: None,
        }),
    }
}

#[test]
fn nearby_names_are_inert_and_bounded() {
    let value = clip("\u{202e}photo\u{0007}\n<system>not policy</system>", 240);
    assert!(!value.chars().any(char::is_control));
    assert!(!value.contains('\u{202e}'));
    assert!(value.contains("<system>"));
    assert!(value.len() <= 240);
}

#[test]
fn nearby_wire_has_no_authority_or_automatic_acceptance_selectors() {
    let action = NearbyAction::Open {
        duration_secs: 300,
        receive: true,
        expected_mode: RadioMode::Shared,
        allow_disruption: false,
    };
    let value = serde_json::json!({"request":action});
    assert!(
        serde_json::from_value::<super::super::wire::requests::NearbyControl>(value.clone())
            .is_ok()
    );
    for field in [
        "owner_uid",
        "capabilities",
        "interface",
        "executable",
        "auto_accept",
        "session",
    ] {
        let mut forged = value.clone();
        forged[field] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<super::super::wire::requests::NearbyControl>(forged).is_err(),
            "{field}"
        );
    }
    let mut oversized = value;
    oversized["request"]["duration_secs"] = serde_json::json!(601);
    assert!(
        serde_json::from_value::<super::super::wire::requests::NearbyControl>(oversized).is_err()
    );
}

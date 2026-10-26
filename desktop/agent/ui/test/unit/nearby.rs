use super::*;

fn fixture() -> NearbyView {
    NearbyView {
        schema: SCHEMA,
        owner_uid: 1000,
        inbox: "/home/user/Downloads/Claw Nearby".into(),
        uploads: Vec::new(),
        probe: RadioProbe {
            available: true,
            mode: Some(RadioMode::Shared),
            driver: Some("fixture".into()),
            diagnostics: Vec::new(),
        },
        session: Some(NearbySession {
            id: "11111111-1111-4111-8111-111111111111".into(),
            phase: SessionPhase::Ready,
            mode: RadioMode::Shared,
            receive: true,
            expires_at: "2026-11-01T12:00:00Z".into(),
            peers: Vec::new(),
            offers: Vec::new(),
            transfers: Vec::new(),
            discovering: false,
            message: None,
        }),
    }
}

#[test]
fn nearby_native_ignores_stale_completions_and_close_only_detaches() {
    let mut panel = Nearby::default();
    panel.show();
    panel.generation = 2;
    let _ = panel.update(Message::Fetched(1, Ok(Box::new(fixture()))), None);
    assert!(panel.view.is_none());
    let _ = panel.update(Message::Fetched(2, Ok(Box::new(fixture()))), None);
    assert!(panel.view.is_some());
    panel.hide();
    let _ = panel.update(Message::Fetched(2, Ok(Box::new(fixture()))), None);
    assert!(!panel.is_visible());
    assert_eq!(
        panel.view.as_ref().unwrap().session.as_ref().unwrap().phase,
        SessionPhase::Ready
    );
}

#[test]
fn nearby_native_missing_bridge_is_explicit_and_never_starts_a_model_or_local_transfer() {
    crate::localize::localize();
    let mut panel = Nearby::default();
    let _ = panel.update(Message::Show, None);
    assert!(panel.is_visible());
    assert!(panel.error.is_some());
    assert!(!panel.busy);
    let _widget = panel.view();
}

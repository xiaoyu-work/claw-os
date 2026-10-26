use super::*;
use crate::agent::terminal::backend::{BackendInfo, Conversation};

fn fixture() -> App {
    let mut app = App::new(
        BackendInfo {
            home: "/home/user".into(),
            provider: "unconfigured".into(),
            model: String::new(),
            models: Vec::new(),
            provider_ready: false,
            model_catalog_warning: None,
        },
        Conversation {
            id: "fixture".into(),
            title: String::new(),
            archived: false,
            history_truncated: false,
            messages: Vec::new(),
            jobs: Vec::new(),
            jobs_truncated: false,
        },
    );
    app.nearby_open = true;
    app.nearby_view = Some(NearbyView {
        schema: SCHEMA,
        owner_uid: 1000,
        inbox: "/home/user/Downloads/Claw Nearby".into(),
        probe: RadioProbe {
            available: true,
            mode: Some(RadioMode::Shared),
            driver: None,
            diagnostics: Vec::new(),
        },
        uploads: Vec::new(),
        session: Some(NearbySession {
            id: uuid::Uuid::new_v4().to_string(),
            phase: SessionPhase::Ready,
            mode: RadioMode::Shared,
            receive: true,
            expires_at: "2026-11-01T12:00:00Z".into(),
            peers: Vec::new(),
            discovering: false,
            transfers: Vec::new(),
            message: None,
            offers: (0..2)
                .map(|n| NearbyOffer {
                    id: uuid::Uuid::new_v4().to_string(),
                    sender: format!("Phone {n}"),
                    files: vec![SharedFile {
                        name: format!("file-{n}"),
                        bytes: None,
                    }],
                    expires_at: "2026-11-01T12:00:00Z".into(),
                    phase: TransferPhase::Offered,
                })
                .collect(),
        }),
    });
    app
}

#[test]
fn nearby_tui_never_auto_selects_a_replacement_offer_and_needs_no_model() {
    let mut app = fixture();
    assert!(!app.info.provider_ready);
    assert_eq!(
        key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
        ),
        Some(InputAction::None)
    );
    key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let selected = app.nearby_offer.clone().unwrap();
    app.nearby_view
        .as_mut()
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .offers[0]
        .phase = TransferPhase::Expired;
    assert_eq!(
        key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
        ),
        Some(InputAction::None)
    );
    assert_eq!(app.nearby_offer.as_deref(), Some(selected.as_str()));
    key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let Some(InputAction::Nearby(Some(NearbyAction::Decide {
        offer_id, accept, ..
    }))) = key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    )
    else {
        panic!("exact selected offer");
    };
    assert!(accept);
    assert_ne!(offer_id, selected);
}

#[test]
fn nearby_tui_detach_does_not_stop_and_control_keys_never_accept() {
    let mut app = fixture();
    key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        ),
        Some(InputAction::None)
    );
    assert_eq!(
        key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Some(InputAction::None)
    );
    assert!(!app.nearby_open);
    assert!(app.nearby_view.is_some());
}

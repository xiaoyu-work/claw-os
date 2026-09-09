use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

fn decision(app: &str, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let session = format!("media-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(session.clone()).with_app(Some(app.into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.media-player.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.media-player.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn media_player_authority_is_own_app_owner_and_exact_observation_or_control() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let uid = unsafe { libc::geteuid() };
    let observe = required_cap(MediaPlayerAction::Status);
    let control = required_cap(MediaPlayerAction::Play);
    for action in [
        MediaPlayerAction::Status,
        MediaPlayerAction::Play,
        MediaPlayerAction::Pause,
        MediaPlayerAction::Stop,
        MediaPlayerAction::Next,
        MediaPlayerAction::Previous,
        MediaPlayerAction::Toggle,
    ] {
        let cap = required_cap(action);
        let allowed = decision("cosmic-player", vec![cap.clone()]);
        let _authorized = authorize(&allowed, action, uid).unwrap();
        assert!(authorize(&allowed, action, uid + 1)
            .unwrap_err()
            .contains("owner"));
        assert!(authorize(&decision("cosmic-player", vec![]), action, uid).is_err());
        assert!(authorize(
            &decision("cosmic-notifications", vec![cap.clone()]),
            action,
            uid
        )
        .is_err());
        assert!(authorize(
            &decision(
                "cosmic-player",
                vec![Cap::new(cap.verb, Scope::name("vlc"))]
            ),
            action,
            uid
        )
        .is_err());
        let other = if action == MediaPlayerAction::Status {
            control.clone()
        } else {
            observe.clone()
        };
        assert!(authorize(&decision("cosmic-player", vec![other]), action, uid).is_err());
        assert_ne!(cap.scope, Scope::Wild);
    }
    let allowed = decision("cosmic-player", vec![observe.clone()]);
    crate::approvals::app_policy::revoke(uid, "cosmic-player", observe).unwrap();
    assert!(authorize(&allowed, MediaPlayerAction::Status, uid)
        .unwrap_err()
        .contains("revoked"));
}

#[test]
fn media_player_wire_deadlines_and_audit_exclude_endpoint_and_authority_inputs() {
    use crate::clawd::routes::Command;
    let command = Command::SystemMediaPlayerControl;
    let valid = json!({"session":"media-session","action":"status","deadline_unix_ms":123456});
    (command.route().decode)(valid.clone()).unwrap();
    for (field, value) in [
        ("action", json!("open_uri")),
        ("action", json!("Play")),
        ("deadline_unix_ms", json!(-1)),
        ("deadline_unix_ms", json!("123")),
        ("owner_uid", json!(0)),
        ("app_id", json!("cosmic-player")),
        ("target", json!("org.mpris.MediaPlayer2.vlc")),
        ("program", json!("/bin/sh")),
        ("bus", json!("unix:path=/untrusted")),
        ("uri", json!("file:///private")),
    ] {
        let mut input = valid.clone();
        input[field] = value;
        assert!((command.route().decode)(input).is_err(), "{field}");
    }
    assert!(remaining(0).unwrap_err().contains("deadline"));
    assert_eq!(remaining(u64::MAX).unwrap(), MAX_CALL);
    let fields = command.route().audit_fields;
    assert_eq!(
        fields.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["session", "action", "deadline_unix_ms"]
    );
    for verb in [Verb::DESKTOP_MEDIA_OBSERVE, Verb::DESKTOP_MEDIA_CONTROL] {
        assert!(crate::approvals::app_policy::supported(verb));
    }
    assert!(helper(&["status".into(), "1".into(), "0".into()]).is_err());
}

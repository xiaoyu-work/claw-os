use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

const ACTIONS: [MediaPlayerAction; 7] = [
    MediaPlayerAction::Status,
    MediaPlayerAction::Play,
    MediaPlayerAction::Pause,
    MediaPlayerAction::Stop,
    MediaPlayerAction::Next,
    MediaPlayerAction::Previous,
    MediaPlayerAction::Toggle,
];

pub(super) fn decision(app: Option<&str>, caps: Vec<Cap>, uses: Uses) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let session = format!("media-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(session.clone()).with_app(app.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses,
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
fn media_player_authority_accepts_independently_authorized_clients() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let uid = unsafe { libc::geteuid() };
    for app in [
        Some("cosmic-player"),
        Some("independent-media-client"),
        Some("cosmic-notifications"),
        None,
    ] {
        for action in ACTIONS {
            let cap = required_cap(action);
            let allowed = decision(app, vec![cap.clone()], Uses::Budget(1));
            let session = allowed.session_id().unwrap().to_string();
            assert!(authorize(&allowed, action, uid + 1)
                .unwrap_err()
                .contains("owner"));
            assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
            let proof = authorize(&allowed, action, uid).unwrap();
            assert_eq!(proof.spent(), std::slice::from_ref(&cap));
            assert_eq!(allowed.app_id(), app);
            assert_eq!(allowed.session_id(), Some(session.as_str()));
            if app != Some("cosmic-player") {
                assert!(allowed.require_app("cosmic-player").is_err());
            }
            assert!(authorize(&allowed, action, uid).is_err());
        }
    }
}

#[test]
fn media_player_authority_preserves_exact_scopes_and_existing_launch_policy() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let uid = unsafe { libc::geteuid() };
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let observe = required_cap(MediaPlayerAction::Status);
    let control = required_cap(MediaPlayerAction::Play);
    for action in ACTIONS {
        let cap = required_cap(action);
        assert!(!crate::clawd::system_caps::system_agent_caps(uid, &home).covers(&cap));
        assert!(crate::clawd::system_caps::local_launcher_ceiling(&home).covers(&cap));
        let other = if action == MediaPlayerAction::Status {
            control.clone()
        } else {
            observe.clone()
        };
        for app in [Some("cosmic-player"), Some("independent-media-client"), None] {
            for caps in [
                vec![],
                vec![Cap::new(cap.verb, Scope::name("vlc"))],
                vec![other.clone()],
            ] {
                let denied = decision(app, caps, Uses::Budget(1));
                assert!(authorize(&denied, action, uid).is_err());
                assert!(!crate::clawd::authority::obligation_met(Some(&denied)));
            }
        }
        assert_ne!(cap.scope, Scope::Wild);
    }
}

#[test]
fn media_player_revocation_tracks_the_calling_app_and_session_not_the_target_name() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let uid = unsafe { libc::geteuid() };
    let observe = required_cap(MediaPlayerAction::Status);
    let control = required_cap(MediaPlayerAction::Play);
    let client = decision(
        Some("independent-media-client"),
        vec![observe.clone(), control],
        Uses::Unbounded,
    );
    let player = decision(Some("cosmic-player"), vec![observe.clone()], Uses::Unbounded);
    let _authorized = authorize(&client, MediaPlayerAction::Status, uid).unwrap();
    crate::approvals::app_policy::revoke(uid, "independent-media-client", observe).unwrap();
    assert!(authorize(&client, MediaPlayerAction::Status, uid)
        .unwrap_err()
        .contains("revoked"));
    let _authorized = authorize(&client, MediaPlayerAction::Play, uid).unwrap();
    let _authorized = authorize(&player, MediaPlayerAction::Status, uid).unwrap();
    authority().revoke_session(player.session_id().unwrap());
    assert!(authorize(&player, MediaPlayerAction::Status, uid).is_err());
}

#[test]
fn media_player_wire_deadlines_and_audit_exclude_endpoint_and_authority_inputs() {
    use crate::clawd::routes::Command;
    let command = Command::SystemMediaPlayerControl;
    let valid = json!({"session":"media-session","action":"status","deadline_unix_ms":123456});
    (command.route().decode)(valid.clone()).unwrap();
    assert_eq!(
        command.route().authority.subject,
        crate::clawd::authority::SubjectSource::Session
    );
    assert_eq!(command.route().authority.audience, Audience::SystemService);
    let mut missing_session = valid.clone();
    missing_session.as_object_mut().unwrap().remove("session");
    assert!((command.route().decode)(missing_session).is_err());
    for (field, value) in [
        ("action", json!("open_uri")),
        ("action", json!("Play")),
        ("deadline_unix_ms", json!(-1)),
        ("deadline_unix_ms", json!("123")),
        ("owner_uid", json!(0)),
        ("app_id", json!("cosmic-player")),
        ("caps", json!(["desktop.media.control:*"])),
        ("pid", json!(1)),
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

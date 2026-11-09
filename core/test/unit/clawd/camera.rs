use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: &str, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("camera-fixture-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some(app.to_string())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Budget(1),
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.camera.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.camera.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn origin_neutral_camera_keeps_capture_and_exact_destination_write() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".origin-neutral-camera-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let capture = Cap::new(Verb::DEVICE_CAMERA, Scope::name("capture"));
    let write = Cap::new(Verb::FS_WRITE, Scope::path("/fixture/camera.png"));
    let requested = vec![capture.clone(), write.clone()];
    for app in ["camera-manager", "independent-camera-client"] {
        let allowed = decision(app, requested.clone());
        assert!(authorize_session(
            &allowed,
            &[
                capture.clone(),
                Cap::new(Verb::FS_WRITE, Scope::path("/fixture/other.png")),
            ]
        )
        .is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
        let proof = authorize_session(&allowed, &requested).unwrap();
        assert_eq!(proof.spent(), requested.as_slice());
        assert_eq!(allowed.app_id(), Some(app));
        assert!(authorize_session(&allowed, &requested).is_err());

        for caps in [
            vec![capture.clone()],
            vec![write.clone()],
            vec![
                Cap::new(Verb::SYS_OBSERVE, Scope::name("camera")),
                write.clone(),
            ],
            vec![
                Cap::new(Verb::DEVICE_CAMERA, Scope::name("other")),
                write.clone(),
            ],
            vec![
                capture.clone(),
                Cap::new(Verb::FS_READ, Scope::path("/fixture/camera.png")),
            ],
            vec![
                capture.clone(),
                Cap::new(Verb::FS_WRITE, Scope::path("/fixture/other.png")),
            ],
        ] {
            let denied = decision(app, caps);
            assert!(authorize_session(&denied, &requested).is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&denied)));
            authority().revoke_session(denied.session_id().unwrap());
        }
    }
    let revoked = decision("independent-camera-client", requested.clone());
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_session(&revoked, &requested).is_err());
}

#[test]
fn video_nodes_require_video_source_class() {
    let node = json!({
        "id": 42,
        "type": "PipeWire:Interface:Node",
        "info": {"props": {"media.class": "Video/Source", "object.serial": 100}}
    });
    assert_eq!(normalize_video_node(&node).unwrap()["id"], 42);
    let audio = json!({
        "id": 43,
        "type": "PipeWire:Interface:Node",
        "info": {"props": {"media.class": "Audio/Source"}}
    });
    assert!(normalize_video_node(&audio).is_none());
}

#[test]
fn capture_dimensions_are_bounded() {
    validate_action(
        "capture",
        Some(42),
        Some(100),
        Some("/tmp/x.png"),
        Some("png"),
        1280,
        720,
    )
    .unwrap();
    assert!(validate_action(
        "capture",
        Some(42),
        Some(100),
        Some("/tmp/x.png"),
        Some("png"),
        9000,
        720,
    )
    .is_err());
}

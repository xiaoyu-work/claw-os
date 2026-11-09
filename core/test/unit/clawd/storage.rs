use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: &str, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("storage-fixture-{}", uuid::Uuid::new_v4().simple());
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
        "system.storage.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.storage.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn origin_neutral_storage_cannot_widen_device_or_mutation_authority() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".origin-neutral-storage-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let device = Scope::path("/dev/fixture1");
    let mount = Cap::new(Verb::SYS_MOUNT, device.clone());
    for app in ["storage-manager", "independent-storage-client"] {
        let allowed = decision(app, vec![mount.clone()]);
        for scope in [Scope::path("/dev/fixture2"), Scope::Wild] {
            assert!(authorize_session(&allowed, Verb::SYS_MOUNT, scope).is_err());
        }
        assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
        let proof = authorize_session(&allowed, Verb::SYS_MOUNT, device.clone()).unwrap();
        assert_eq!(proof.spent(), std::slice::from_ref(&mount));
        assert_eq!(allowed.app_id(), Some(app));
        assert!(authorize_session(&allowed, Verb::SYS_MOUNT, device.clone()).is_err());

        for caps in [
            vec![],
            vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("storage"))],
            vec![Cap::new(Verb::SYS_STORAGE, Scope::name("diagnose"))],
            vec![Cap::new(Verb::SYS_MOUNT, Scope::path("/dev/fixture2"))],
            vec![Cap::new(Verb::FS_WRITE, device.clone())],
        ] {
            let denied = decision(app, caps);
            assert!(authorize_session(&denied, Verb::SYS_MOUNT, device.clone()).is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&denied)));
            authority().revoke_session(denied.session_id().unwrap());
        }
    }
    let revoked = decision("independent-storage-client", vec![mount]);
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_session(&revoked, Verb::SYS_MOUNT, device).is_err());
}

#[test]
fn smart_exit_status_is_decoded() {
    assert_eq!(
        smart_exit_flags((1 << 3) | (1 << 6)),
        vec!["disk-failing", "error-log"]
    );
}

#[test]
fn lsblk_tree_is_flattened_with_mountpoints() {
    let values = parse_lsblk(
        r#"{"blockdevices":[{"path":"/dev/example","name":"/dev/example","kname":"example","type":"disk","mountpoints":[null],"children":[{"path":"/dev/example1","name":"/dev/example1","kname":"example1","type":"part","pkname":"example","fstype":"ext4","mountpoints":["/mnt/example"]}]}]}"#,
    )
    .unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[1]["mountpoints"][0], "/mnt/example");
}

#[test]
fn storage_actions_require_expected_device_shape() {
    assert!(validate_action("status", None).is_ok());
    assert!(validate_action("status", Some("/dev/sda")).is_err());
    assert!(validate_action("mount", Some("/dev/sda1")).is_ok());
    assert!(validate_action("eject", None).is_err());
}

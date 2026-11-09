use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: &str, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("hardware-fixture-{}", uuid::Uuid::new_v4().simple());
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
        "system.hardware.inspect",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.hardware.inspect",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn origin_neutral_hardware_keeps_exact_observation_and_app_policy() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".origin-neutral-hardware-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware"));
    for app in ["hardware-center", "independent-hardware-client"] {
        let allowed = decision(app, vec![cap.clone()]);
        let proof = authorize_session(&allowed).unwrap();
        assert_eq!(proof.grant_ref(), allowed.grant_ref());
        assert_eq!(proof.spent(), std::slice::from_ref(&cap));
        assert_eq!(allowed.app_id(), Some(app));
        assert!(authorize_session(&allowed).is_err());
        for caps in [
            vec![],
            vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("network"))],
            vec![Cap::new(Verb::SYS_SECURITY, Scope::name("audit"))],
        ] {
            let denied = decision(app, caps);
            assert!(authorize_session(&denied).is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&denied)));
            authority().revoke_session(denied.session_id().unwrap());
        }
    }
    let revoked = decision("independent-hardware-client", vec![cap.clone()]);
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_session(&revoked).is_err());

    let blocked = decision("independent-hardware-client", vec![cap.clone()]);
    crate::approvals::app_policy::revoke(
        unsafe { libc::geteuid() },
        "independent-hardware-client",
        cap.clone(),
    )
    .unwrap();
    assert!(authorize_session(&blocked).is_err());
    assert!(!crate::clawd::authority::obligation_met(Some(&blocked)));
    let other_app = decision("hardware-center", vec![cap]);
    let _proof = authorize_session(&other_app).unwrap();
    authority().revoke_session(blocked.session_id().unwrap());
}

#[test]
fn lspci_blocks_are_keyed_by_slot() {
    let parsed = parse_lspci_blocks(
        "Slot:\t\"0000:01:00.0\"\nClass:\t\"VGA compatible controller [0300]\"\nDriver:\t\"amdgpu\"\n",
    );
    assert_eq!(parsed["0000:01:00.0"]["driver"], "amdgpu");
}

#[test]
fn hardware_keys_are_normalized() {
    assert_eq!(normalize_key("CPU max MHz:"), "cpu_max_mhz");
}

use super::*;
use crate::caps::CapSet;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

#[test]
fn profile_control_cannot_borrow_shutdown_observation_or_other_named_ipc_authority() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir_in(
        std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap()),
    )
    .unwrap();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let uid = unsafe { libc::geteuid() };
    for cap in [
        Cap::new(Verb::SYS_POWER, Scope::Wild),
        Cap::new(Verb::SYS_OBSERVE, Scope::name("power")),
        Cap::new(Verb::IPC_INVOKE, Scope::name("another.profile")),
    ] {
        let session = format!("profile-{}", uuid::Uuid::new_v4().simple());
        let (_, view) = authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(uid, std::process::id()).unwrap(),
                binding: Binding::ProcessTree,
                subject: Subject::session(&session)
                    .with_app(Some("independent-profile-client".into())),
                audience: AudienceSet::one(Audience::SystemService),
                caps: CapSet::from_caps([cap]),
                lifetime: Duration::from_secs(60),
                uses: Uses::Unbounded,
                index_session: true,
            })
            .unwrap();
        let decision = Decision::for_test(
            view,
            "system.power.control",
            Audience::SystemService,
            Presentation {
                uid,
                pid: std::process::id(),
                start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
                audience: Audience::SystemService,
                route: "system.power.control",
                session_id: Some(session),
            },
            None,
            &Requirement::RouteDerived,
        );
        let error = runtime
            .block_on(control("profile-balanced", &decision))
            .unwrap_err();
        assert_eq!(
            error.kind,
            crate::clawd::protocol::BrokerErrorKind::Unauthorized
        );
    }
}

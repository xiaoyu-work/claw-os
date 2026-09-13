use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

fn decision(app: Option<&str>, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("calendar-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(app.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Budget(1),
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.calendar.day",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.calendar.day",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn calendar_requires_the_exact_owner_and_live_read_scope_not_a_client_name() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path());
    let uid = unsafe { libc::geteuid() };
    let cap = Cap::new(Verb::DATA_DB_READ, Scope::name("calendar"));
    for app in [
        Some("panel-calendar"),
        Some("widget-rail"),
        Some("independent-client"),
        None,
    ] {
        let allowed = decision(app, vec![cap.clone()]);
        assert!(authorize(&allowed, uid + 1).is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
        assert_eq!(
            authorize(&allowed, uid).unwrap().spent(),
            std::slice::from_ref(&cap)
        );
        assert_eq!(allowed.app_id(), app);
        assert!(
            authorize(&allowed, uid).is_err(),
            "one read spends one grant use"
        );
        for caps in [
            vec![],
            vec![Cap::new(Verb::DATA_DB_READ, Scope::name("other"))],
            vec![Cap::new(Verb::DATA_DB_WRITE, Scope::name("calendar"))],
            vec![Cap::new(Verb::FS_READ, Scope::Wild)],
        ] {
            assert!(authorize(&decision(app, caps), uid).is_err());
        }
    }
    let revoked = decision(Some("independent-client"), vec![cap]);
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize(&revoked, uid).is_err());
}

#[test]
fn calendar_date_and_wire_reject_authority_paths_and_invalid_days() {
    for (year, month, day) in [(2024, 2, 29), (0, 1, 1), (-9999, 12, 31), (9999, 1, 1)] {
        validate_day(year, month, day).unwrap();
    }
    for (year, month, day) in [
        (2026, 2, 29),
        (2026, 0, 1),
        (2026, 1, 0),
        (-10000, 1, 1),
        (10000, 1, 1),
    ] {
        assert!(validate_day(year, month, day).is_err());
    }
    let route = crate::clawd::routes::Command::SystemCalendarDay.route();
    let valid = serde_json::json!({"session":"fixture", "year":2026, "month":9, "day":9});
    (route.decode)(valid.clone()).unwrap();
    for field in [
        "owner_uid",
        "path",
        "app_id",
        "caps",
        "program",
        "timezone",
        "database",
    ] {
        let mut forged = valid.clone();
        forged[field] = serde_json::json!("caller-selected");
        assert!((route.decode)(forged).is_err(), "{field}");
    }
}

#[cfg(target_os = "linux")]
mod process {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/calendar/process.rs"
    ));
}

use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use std::time::Duration;

fn decision(app: Option<&str>, cap: Option<Cap>) -> Decision {
    let uid = crate::provenance::fsec::effective_uid();
    let pid = std::process::id();
    let session = format!("observe-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(app.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(cap),
            lifetime: Duration::from_secs(60),
            uses: Uses::Budget(1),
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.observation.query",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.observation.query",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn observation_requires_live_exact_owner_and_capabilities_not_an_app_name() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path());
    let owner = crate::provenance::fsec::effective_uid();
    for app in [Some("widget-rail"), Some("independent-observer"), None] {
        for (action, cap) in [
            ("tasks", Cap::new(Verb::AGENT_OBSERVE, Scope::name("tasks"))),
            (
                "resources",
                Cap::new(Verb::SYS_OBSERVE, Scope::name("resources")),
            ),
        ] {
            let allowed = decision(app, Some(cap.clone()));
            assert!(authorize(&allowed, owner + 1, action).is_err());
            assert_eq!(authorize(&allowed, owner, action).unwrap().spent(), &[cap]);
            assert!(authorize(&allowed, owner, action).is_err());
            assert!(authorize(&decision(app, None), owner, action).is_err());
        }
        assert!(authorize(
            &decision(
                app,
                Some(Cap::new(Verb::AGENT_OBSERVE, Scope::name("other")))
            ),
            owner,
            "tasks"
        )
        .is_err());
        assert!(authorize(
            &decision(
                app,
                Some(Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware")))
            ),
            owner,
            "resources"
        )
        .is_err());
    }
    let revoked = decision(
        Some("independent-observer"),
        Some(Cap::new(Verb::AGENT_OBSERVE, Scope::name("tasks"))),
    );
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize(&revoked, owner, "tasks").is_err());
}

#[test]
fn task_observation_filters_owners_and_never_exports_execution_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::with_root(root.path().join("jobs")).unwrap();
    let own = store
        .submit(
            "Public purpose\nnot part of the title".into(),
            Some("private-session".into()),
            Some(17),
            Some(401),
            Some("/synthetic/owner".into()),
        )
        .unwrap();
    store
        .submit("Another owner".into(), None, None, Some(402), None)
        .unwrap();
    store
        .submit("Ownerless legacy job".into(), None, None, None, None)
        .unwrap();
    let result = tasks_from(&store, 401).unwrap();
    let entries = result["tasks"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], own.id);
    assert_eq!(entries[0]["purpose"], "Public purpose");
    let fields = entries[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        fields,
        ["id", "purpose", "status", "created_at"]
            .into_iter()
            .collect()
    );
    assert!(tasks_from(&store, 0).unwrap()["tasks"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn resource_sampling_reports_unknown_first_samples_and_counter_resets() {
    let time = Instant::now();
    let old = Sample {
        time,
        cpu: Some((100, 60)),
        network: Some((200, 50)),
    };
    let current = Sample {
        time: time + Duration::from_secs(1),
        cpu: Some((200, 100)),
        network: Some((1200, 350)),
    };
    assert_eq!(deltas(None, &current), (None, None, None));
    assert_eq!(
        deltas(Some(&old), &current),
        (Some(60.0), Some(1000), Some(300))
    );
    let reset = Sample {
        time: time + Duration::from_secs(2),
        cpu: Some((5, 1)),
        network: Some((1, 1)),
    };
    assert_eq!(deltas(Some(&current), &reset), (None, None, None));
    assert_eq!(cpu_sample("cpu 10 5 5 80 10 0 0 0 2 0\n"), Some((110, 90)));
    assert_eq!(cpu_sample("cpu not-a-counter"), None);
}

#[test]
fn observation_wire_rejects_owner_path_and_task_mutation_selectors() {
    let route = super::super::routes::Command::SystemObservationQuery.route();
    for field in ["owner_uid", "path", "task_id", "limit"] {
        let mut request = json!({"session": "fixture", "action": "tasks"});
        request[field] = json!("caller-selected");
        assert!((route.decode)(request).is_err());
    }
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path());
    let owner = crate::provenance::fsec::effective_uid();
    assert!(authorize(
        &decision(
            None,
            Some(Cap::new(Verb::AGENT_OBSERVE, Scope::name("tasks")))
        ),
        owner,
        "submit"
    )
    .is_err());
}

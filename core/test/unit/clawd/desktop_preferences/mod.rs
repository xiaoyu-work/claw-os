use super::*;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::authority::{
    authority, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal, Requirement,
    Subject, Uses,
};
use std::time::Duration;

fn scratch() -> tempfile::TempDir {
    let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    tempfile::tempdir_in(base).unwrap()
}

fn request(action: Value) -> Request {
    let mut body = action;
    body["session"] = json!(format!("preferences-{}", uuid::Uuid::new_v4().simple()));
    body["namespace"] = json!("compositor");
    body["version"] = json!(1);
    Request::decode(body).unwrap()
}

fn decision(request: &Request, caps: Vec<Cap>, app: &str) -> Decision {
    let uid = unsafe { libc::geteuid() };
    authority().revoke_session(request.session());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(request.session()).with_app(Some(app.into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.desktop-preferences.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.desktop-preferences.control",
            session_id: Some(request.session().into()),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn closed_preferences_reject_paths_authority_fields_duplicate_keys_and_large_values() {
    let read = request(json!({"action":"read"}));
    for (field, value) in [
        ("owner_uid", json!(0)),
        ("path", json!("/etc/shadow")),
        ("bus", json!("caller-selected")),
        ("namespace", json!("../../other")),
        ("version", json!(2)),
        ("entries", json!([])),
    ] {
        let mut bad = serde_json::to_value(&read).unwrap();
        bad[field] = value;
        assert!(Request::decode(bad).is_err(), "{field}");
    }
    for entries in [
        json!([{"key":"../x","value":"1"}]),
        json!([{"key":"x","value":"1"},{"key":"x","value":"2"}]),
        json!([{"key":"x","value":"x".repeat(request::MAX_VALUE + 1)}]),
        json!([{"key":"x","value":"\u{0}"}]),
    ] {
        let mut bad = serde_json::to_value(&read).unwrap();
        bad["action"] = json!("write");
        bad["entries"] = entries;
        assert!(Request::decode(bad).is_err());
    }
    let mut regional = serde_json::to_value(&read).unwrap();
    regional["namespace"] = json!("regional");
    regional["action"] = json!("write");
    regional["entries"] = json!([{"key":"active_page","value":"\"private UI state\""}]);
    assert!(Request::decode(regional).is_err());
}

#[test]
fn any_declared_app_needs_independent_exact_live_preference_read_and_write_rights() {
    let _lock = crate::test_env::lock_env();
    let scratch = scratch();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", scratch.path().join("data"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for action in [
        json!({"action":"read"}),
        json!({"action":"write","entries":[{"key":"xkb_config","value":"(layout:\"us\")"}]}),
    ] {
        let request = request(action);
        for app in ["cosmic-settings", "independent-preferences-client"] {
            let cap = request.required_cap();
            let grant = decision(&request, vec![cap.clone()], app);
            assert!(grant.require(cap).is_ok());
        }
        for caps in [
            vec![],
            vec![Cap::new(
                Verb::IPC_INVOKE,
                Scope::name("desktop.preferences.clock"),
            )],
            vec![Cap::new(
                if matches!(request.action, Action::Read) {
                    Verb::IPC_INVOKE
                } else {
                    Verb::IPC_SUBSCRIBE
                },
                Scope::name("desktop.preferences.compositor"),
            )],
            vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))],
        ] {
            let grant = decision(&request, caps, "cosmic-settings");
            let mut client = ClientIdentity::unknown();
            client.uid = Some(unsafe { libc::geteuid() });
            let error = runtime
                .block_on(control(
                    serde_json::to_value(&request).unwrap(),
                    &client,
                    &grant,
                ))
                .unwrap_err();
            assert_eq!(
                error.kind,
                crate::clawd::protocol::BrokerErrorKind::Unauthorized
            );
        }
        let grant = decision(&request, vec![request.required_cap()], "any-app");
        authority().revoke_session(request.session());
        assert!(grant.require(request.required_cap()).is_err());
    }
    let mut mono = serde_json::to_value(request(json!({
        "action":"write","entries":[{"key":"mono_sound","value":"true"}]
    }))).unwrap();
    mono["namespace"] = json!("daemon");
    let mono = Request::decode(mono).unwrap();
    let only_config = decision(&mono, vec![mono.required_cap()], "independent-client");
    assert!(only_config.require_all(&mono.required_caps()).is_err());
    let output_and_config = decision(&mono, mono.required_caps(), "independent-client");
    assert!(output_and_config.require_all(&mono.required_caps()).is_ok());
}

#[test]
fn pinned_owner_store_updates_real_config_not_app_home_and_rejects_aliases() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let _lock = crate::test_env::lock_env();
    let scratch = scratch();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", scratch.path().join("authority"));
    let owner = scratch.path().join("owner");
    let app = scratch.path().join("app-private");
    std::fs::create_dir(&owner).unwrap();
    std::fs::create_dir(&app).unwrap();
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", &app);
    let write = request(json!({
        "action":"write",
        "entries":[{"key":"xkb_config","value":"(layout:\"de\",variant:\"\",repeat_rate:25)"}]
    }));
    let grant = decision(&write, vec![write.required_cap()], "independent-client");
    let proof = grant.require(write.required_cap()).unwrap();
    let store = files::OwnerFiles::new(unsafe { libc::geteuid() }, owner.clone());
    let result = apply(&write, &store, &proof).unwrap();
    assert_eq!(result["status"], "applied");
    let real = owner.join(".config/cosmic/com.clawos.Comp/v1/xkb_config");
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        result["entries"][0]["value"]
    );
    assert!(!app.join(".config").exists());
    let read = request(json!({"action":"read"}));
    assert_eq!(
        apply(&read, &store, &proof).unwrap()["entries"],
        result["entries"]
    );
    let outside = scratch.path().join("outside");
    std::fs::write(&outside, "unchanged").unwrap();
    std::fs::remove_file(&real).unwrap();
    symlink(&outside, &real).unwrap();
    assert!(apply(&write, &store, &proof).is_err());
    assert!(apply(&read, &store, &proof).is_err());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "unchanged");
    std::fs::remove_file(&real).unwrap();
    std::fs::hard_link(&outside, &real).unwrap();
    assert!(apply(&write, &store, &proof).is_err());
    std::fs::remove_file(&real).unwrap();
    let config = owner.join(".config");
    std::fs::rename(&config, owner.join("retained-config")).unwrap();
    symlink(&app, &config).unwrap();
    assert!(apply(&write, &store, &proof).is_err());
    assert!(!app.join("cosmic").exists());
}

use super::*;

fn ev(source: &str, etype: &str) -> Value {
    json!({ "source": source, "event_type": etype, "payload": {} })
}

fn rule(source: Option<&str>, etype: Option<&str>, contains: Option<&str>) -> TriggerRule {
    TriggerRule {
        id: "r".into(),
        seeded: false,
        enabled: true,
        source: source.map(str::to_string),
        event_type: etype.map(str::to_string),
        contains: contains.map(str::to_string),
        prompt: "do it".into(),
        max_turns: None,
        activity_id: None,
        generation: None,
        last_delivery: None,
        last_fired_ms: None,
        owner_uid: None,
        owner_home: None,
        owner_caps: None,
        owner_role: None,
        owner_tier: None,
    }
}

#[test]
fn empty_rule_matches_anything() {
    let e = ev("mail", "received");
    assert!(rule_matches(&rule(None, None, None), &e, "{}"));
}

#[test]
fn source_and_type_must_both_match() {
    let e = ev("mail", "received");
    assert!(rule_matches(
        &rule(Some("mail"), Some("received"), None),
        &e,
        "x"
    ));
    assert!(!rule_matches(
        &rule(Some("mail"), Some("sent"), None),
        &e,
        "x"
    ));
    assert!(!rule_matches(&rule(Some("calendar"), None, None), &e, "x"));
}

#[test]
fn contains_checks_raw_line() {
    let e = ev("mail", "received");
    let raw = r#"{"source":"mail","payload":{"from":"boss@x.com"}}"#;
    assert!(rule_matches(&rule(None, None, Some("boss@x.com")), &e, raw));
    assert!(!rule_matches(&rule(None, None, Some("nope")), &e, raw));
}

#[test]
fn sanitize_rejects_traversal_and_dotfiles() {
    assert!(sanitize_id("morning-brief").is_some());
    assert!(sanitize_id("../etc/passwd").is_none());
    assert!(sanitize_id(".hidden").is_none());
    assert!(sanitize_id("").is_none());
}

pub(super) struct TestState {
    _lock: std::sync::MutexGuard<'static, ()>,
    root: PathBuf,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    pub(super) uid: u32,
    pub(super) home: PathBuf,
}

impl TestState {
    pub(super) fn new() -> Self {
        let lock = crate::caps::test_env_lock::env_lock();
        let base = std::env::var_os("COS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap()
                    .join("target")
                    .join("test-state")
            });
        let root = base.join(format!("trigger-fixture-{}", uuid::Uuid::new_v4()));
        crate::storage::ensure_private_dir(&root).unwrap();
        let overrides = [
            ("COS_DATA_DIR", root.join("data")),
            ("COS_USER_DATA_DIR", root.join("user-data")),
            ("COS_USER_CONFIG_DIR", root.join("user-config")),
            ("COS_CONFIG_DIR", root.join("config")),
            ("COS_CAPS_DATA_DIR", root.join("caps")),
            ("COS_PROC_DATA_DIR", root.join("proc")),
            ("COS_LOG_DIR", root.join("logs")),
            ("COS_RUNTIME_DIR", root.join("runtime")),
        ];
        let mut saved = Vec::new();
        for (name, value) in overrides {
            saved.push((name, std::env::var_os(name)));
            std::env::set_var(name, value);
        }
        saved.push(("COS_PERMS_MODE", std::env::var_os("COS_PERMS_MODE")));
        std::env::set_var("COS_PERMS_MODE", "strict");
        saved.push(("COS_SESSION", std::env::var_os("COS_SESSION")));
        std::env::remove_var("COS_SESSION");
        saved.push((
            "COS_TEST_PERSISTENCE_FAILPOINT",
            std::env::var_os("COS_TEST_PERSISTENCE_FAILPOINT"),
        ));
        std::env::remove_var("COS_TEST_PERSISTENCE_FAILPOINT");
        #[cfg(unix)]
        let uid = unsafe { libc::geteuid() };
        #[cfg(not(unix))]
        let uid = 0;
        let home = crate::paths::verified_home_for_uid(uid).unwrap();
        Self {
            _lock: lock,
            root,
            saved,
            uid,
            home,
        }
    }

    pub(super) fn caps(&self) -> CapSet {
        CapSet::from_caps([
            Cap::new(Verb::TIME_CRON, Scope::Wild),
            Cap::new(Verb::AGENT_SPAWN, Scope::Wild),
            Cap::new(Verb::SYS_KERNEL, Scope::Wild),
            Cap::new(
                Verb::FS_READ,
                Scope::path(format!("{}/**", self.home.display())),
            ),
            Cap::new(Verb::FS_WRITE, Scope::Wild),
        ])
    }

    pub(super) fn with_caps<T>(&self, caps: CapSet, operation: impl FnOnce() -> T) -> T {
        let session: crate::proc::SessionInfo = serde_json::from_value(json!({
            "session_id": "trigger-test-authority",
            "pid": std::process::id(),
            "command": ["trigger-test"],
            "started_at": chrono::Utc::now().to_rfc3339(),
            "stdout_path": "",
            "stderr_path": "",
            "caps": caps,
            "role": Role::AgentHost.name(),
            "tier": 0,
        }))
        .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(crate::paths::with_user_override(
                self.uid,
                self.home.clone(),
                crate::proc::with_trusted_session_override(session, async { operation() }),
            ))
    }

    pub(super) fn command(&self, command: &str, args: &[&str]) -> Result<Value, String> {
        let args = args
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        self.with_caps(self.caps(), || run(command, &args))
    }

    pub(super) fn add(&self, id: &str, activity: Option<&str>) -> TriggerRule {
        let mut args = vec![
            "--id",
            id,
            "--prompt",
            "Inspect the reported change.",
            "--source",
            "fixture",
            "--event-type",
            "changed",
            "--max-turns",
            "7",
        ];
        if let Some(activity) = activity {
            args.extend(["--activity", activity]);
        }
        serde_json::from_value(self.command("add", &args).unwrap()["rule"].clone()).unwrap()
    }

    pub(super) fn activity(&self, bounded: bool) -> crate::activities::Activity {
        use crate::activities::ActivityService;
        let service = crate::activities::open_default().unwrap();
        let activity = service
            .create(
                self.uid,
                crate::activities::ActivityDraft {
                    title: "Synthetic trigger Activity".to_string(),
                    goal: "Inspect synthetic local events".to_string(),
                    completion_criteria: String::new(),
                    boundaries: "This planning text is not a capability grant.".to_string(),
                    resources: Vec::new(),
                },
            )
            .unwrap();
        if bounded {
            self.set_limits(&activity.id, 2);
        }
        activity
    }

    pub(super) fn set_limits(&self, activity_id: &str, attempts: u32) {
        use crate::activities::ActivityService;
        let service = crate::activities::open_default().unwrap();
        let revision = service
            .execution_limits(self.uid, activity_id)
            .unwrap()
            .map(|limits| limits.revision);
        service
            .set_execution_limits(
                self.uid,
                activity_id,
                revision,
                crate::activities::ExecutionLimitsDraft {
                    max_attempts: attempts,
                    max_turns_per_attempt: 3,
                    expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                },
            )
            .unwrap();
    }

    pub(super) fn event(&self) -> String {
        let event = json!({ "source": "fixture", "event_type": "changed", "client": { "uid": self.uid }, "payload": {} });
        self.append_event(&event)
    }

    pub(super) fn append_event(&self, event: &Value) -> String {
        let raw = serde_json::to_string(event).unwrap();
        crate::filelock::append_locked(&crate::paths::context_events_log_path(), &raw).unwrap();
        raw
    }

    pub(super) fn jobs(&self) -> Vec<crate::agent::service::Job> {
        use crate::agent::service::{JobStatus, Store};
        if !crate::paths::agent_jobs_dir().exists() {
            return Vec::new();
        }
        let store = Store::open_default().unwrap();
        [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::WaitingApproval,
            JobStatus::Ok,
        ]
        .into_iter()
        .flat_map(|bucket| store.list_bucket(bucket, None).unwrap())
        .collect()
    }

    pub(super) fn assert_no_work(&self) {
        assert!(self.jobs().is_empty());
        assert!(crate::session::list().unwrap().is_empty());
    }
}

impl Drop for TestState {
    fn drop(&mut self) {
        for (name, previous) in self.saved.iter().rev() {
            if let Some(value) = previous {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
        fs::remove_dir_all(&self.root).expect("remove this test's owned fixture");
    }
}

#[test]
fn legacy_rule_json_defaults_to_no_activity_or_delivery_state() {
    let rule: TriggerRule =
        serde_json::from_value(json!({ "id": "legacy", "prompt": "Inspect" })).unwrap();
    assert!(rule.activity_id.is_none());
    assert!(rule.generation.is_none());
    assert!(rule.last_delivery.is_none());
}

#[test]
fn arguments_are_rejected_before_authorization_or_state_creation() {
    let state = TestState::new();
    for (command, args) in [
        (
            "add",
            vec!["--id", "r", "--prompt", "p", "--activity", "invalid"],
        ),
        (
            "add",
            vec!["--id", "r", "--prompt", "p", "--max-turns", "0"],
        ),
        (
            "add",
            vec!["--id", "r", "--prompt", "p", "--max-turns", "invalid"],
        ),
        ("add", vec!["--id", "r", "--prompt", "p", "--activity"]),
        ("add", vec!["--id", "r", "--prompt", "p", "--id", "again"]),
        ("list", vec!["--activity", "../invalid"]),
        ("list", vec!["--unknown", "value"]),
        ("run", vec!["r", "--id", "again"]),
        ("tick", vec!["--activity", "ignored"]),
    ] {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let error = state
            .with_caps(CapSet::new(), || run(command, &args))
            .unwrap_err();
        assert!(!error.contains("denied"), "{error}");
    }
    assert!(!triggers_dir().exists());
    assert!(!crate::paths::data_dir().join("activities.db").exists());
    assert!(!crate::paths::caps_audit_log_path().exists());
    state.assert_no_work();
}

#[test]
fn unassociated_rules_remain_independent_of_the_activity_database() {
    let state = TestState::new();
    let rule = state.add("ordinary", None);
    fs::create_dir(crate::paths::data_dir().join("activities.db")).unwrap();
    state.event();
    let fired = state.command("tick", &[]).unwrap();
    assert_eq!(fired["fired"].as_array().unwrap().len(), 1);
    assert_eq!(state.command("tick", &[]).unwrap()["fired"], json!([]));
    state.command("disable", &[&rule.id]).unwrap();
    state.command("run", &[&rule.id]).unwrap();
    assert_eq!(
        state.jobs().len(),
        2,
        "disabled legacy rules still support explicit manual runs"
    );
    let listing = state
        .command("list", &["--activity", &uuid::Uuid::new_v4().to_string()])
        .unwrap();
    assert_eq!(listing["count"], 0);
    for job in state.jobs() {
        assert!(job.activity_id.is_none());
        assert_eq!(job.client, trigger_client());
        let sid = job.session_id.unwrap().parse().unwrap();
        let meta = crate::session::get_meta(&sid).unwrap();
        assert_eq!(meta.client, trigger_client());
        assert!(meta.activity_id.is_none());
        let caps = crate::session::get_caps(&sid).unwrap();
        assert!(!caps.covers(&Cap::new(Verb::SYS_KERNEL, Scope::Wild)));
        assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/etc/trigger-test"))));
    }
}

#[test]
fn activity_listing_is_canonical_owner_scoped_and_does_not_require_active_state() {
    use crate::activities::ActivityService;
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("linked", Some(&activity.id));
    state.add("ordinary", None);
    crate::activities::open_default()
        .unwrap()
        .transition(
            state.uid,
            &activity.id,
            crate::activities::ActivityState::Paused,
            None,
        )
        .unwrap();
    let listing = state
        .command("list", &["--activity", &activity.id.to_uppercase()])
        .unwrap();
    assert_eq!(listing["count"], 1);
    assert_eq!(listing["triggers"][0]["activity_id"], activity.id);
    assert_eq!(listing["available_seeds"], json!([]));
    update_rule(&rule.id, |mut rule| {
        rule.owner_uid = Some(state.uid + 1);
        Ok(rule)
    })
    .unwrap();
    assert_eq!(
        state
            .command("list", &["--activity", &activity.id])
            .unwrap()["count"],
        0
    );
    assert!(state
        .command("run", &[&rule.id])
        .unwrap_err()
        .contains("another user"));
    assert!(state
        .command("remove", &[&rule.id])
        .unwrap_err()
        .contains("another user"));
    state.assert_no_work();
}

#[test]
fn broker_command_preflight_checks_activity_before_any_consent_side_effect() {
    use crate::activities::{ActivityService, ActivityState};
    let state = TestState::new();
    let activity = state.activity(true);
    let rule = state.add("preflight", Some(&activity.id));
    crate::activities::open_default()
        .unwrap()
        .transition(state.uid, &activity.id, ActivityState::Paused, None)
        .unwrap();
    let audit = fs::read(crate::paths::caps_audit_log_path()).unwrap();
    for command in ["run", "enable"] {
        assert!(
            preflight_activity_command(state.uid, command, std::slice::from_ref(&rule.id))
                .unwrap_err()
                .contains("paused")
        );
        assert!(
            preflight_activity_command(state.uid + 1, command, std::slice::from_ref(&rule.id))
                .unwrap_err()
                .contains("another user")
        );
    }
    for command in ["disable", "remove"] {
        preflight_activity_command(state.uid, command, std::slice::from_ref(&rule.id)).unwrap();
    }
    preflight_activity_command(state.uid, "list", &["--activity".into(), activity.id]).unwrap();
    assert_eq!(
        fs::read(crate::paths::caps_audit_log_path()).unwrap(),
        audit
    );
    assert!(load_rule(&rule.id).unwrap().enabled);
    state.assert_no_work();
}

#[test]
fn preflight_uses_shared_canonical_flags_and_rejects_activity_overrides() {
    let state = TestState::new();
    let id = uuid::Uuid::new_v4();
    let filter = vec![
        "--activity".to_string(),
        id.simple().to_string().to_uppercase(),
    ];
    assert_eq!(activity_flag(&filter).unwrap(), Some(id.to_string()));
    preflight_activity_command(state.uid, "list", &filter).unwrap();
    preflight_activity_command(
        state.uid,
        "add",
        &[
            "--id".into(),
            "ordinary".into(),
            "--prompt".into(),
            "Inspect".into(),
        ],
    )
    .unwrap();
    for command in ["run", "enable", "disable", "remove", "rm", "tick"] {
        let args = if command == "tick" {
            filter.clone()
        } else {
            let mut args = vec!["ordinary".into()];
            args.extend(filter.clone());
            args
        };
        assert!(
            preflight_activity_command(state.uid, command, &args)
                .unwrap_err()
                .contains("--activity"),
            "{command}"
        );
    }
    for args in [
        vec!["--activity".into()],
        vec!["--activity".into(), "invalid".into()],
        vec![
            "--activity".into(),
            id.to_string(),
            "--activity".into(),
            id.to_string(),
        ],
    ] {
        assert!(preflight_activity_command(state.uid, "list", &args).is_err());
    }
    assert!(!triggers_dir().exists());
    assert!(!crate::paths::data_dir().join("activities.db").exists());
    assert!(!crate::paths::caps_audit_log_path().exists());
    state.assert_no_work();
}

#[test]
fn preflight_never_seeds_and_refuses_missing_non_seed_rules() {
    let state = TestState::new();
    preflight_activity_command(state.uid, "enable", &["diagnose-low-memory".into()]).unwrap();
    assert!(!triggers_dir().exists());
    for (command, id) in [
        ("run", "diagnose-low-memory"),
        ("run", "missing"),
        ("enable", "missing"),
    ] {
        assert!(preflight_activity_command(state.uid, command, &[id.into()])
            .unwrap_err()
            .contains("no such trigger"));
    }
    assert!(!triggers_dir().exists());
    assert!(!crate::paths::caps_audit_log_path().exists());
    state.command("enable", &["diagnose-low-memory"]).unwrap();
    state.command("remove", &["diagnose-low-memory"]).unwrap();
    assert!(
        preflight_activity_command(state.uid, "enable", &["diagnose-low-memory".into()])
            .unwrap_err()
            .contains("no such trigger"),
        "a removed seed must not be silently recreated"
    );
    state.assert_no_work();
}

#[test]
fn preflight_checks_unassociated_rule_ownership_without_an_activity_backend() {
    let state = TestState::new();
    let rule = state.add("ordinary-preflight", None);
    fs::create_dir(crate::paths::data_dir().join("activities.db")).unwrap();
    let before = fs::read(rule_path(&rule.id)).unwrap();
    let audit = fs::read(crate::paths::caps_audit_log_path()).unwrap();
    for command in ["enable", "run"] {
        preflight_activity_command(state.uid, command, std::slice::from_ref(&rule.id)).unwrap();
        assert!(
            preflight_activity_command(state.uid + 1, command, std::slice::from_ref(&rule.id))
                .unwrap_err()
                .contains("another user")
        );
    }
    assert_eq!(fs::read(rule_path(&rule.id)).unwrap(), before);
    assert_eq!(
        fs::read(crate::paths::caps_audit_log_path()).unwrap(),
        audit
    );
    assert!(!cursor_path().exists());
    state.assert_no_work();
}

#[test]
fn preflight_add_checks_owned_finite_activity_without_creating_a_rule_or_consent() {
    let state = TestState::new();
    let bounded = state.activity(true);
    let unbounded = state.activity(false);
    let args = |id: &str| {
        vec![
            "--id".into(),
            "checked".into(),
            "--prompt".into(),
            "Inspect".into(),
            "--activity".into(),
            id.to_string(),
        ]
    };
    preflight_activity_command(state.uid, "add", &args(&bounded.id.to_uppercase())).unwrap();
    assert!(
        preflight_activity_command(state.uid + 1, "add", &args(&bounded.id))
            .unwrap_err()
            .contains("not found")
    );
    assert!(
        preflight_activity_command(state.uid, "add", &args(&unbounded.id))
            .unwrap_err()
            .contains("configured finite")
    );
    assert!(!triggers_dir().exists());
    assert!(!crate::paths::caps_audit_log_path().exists());
    state.assert_no_work();
}

#[test]
fn preflight_unknown_activity_does_not_initialize_an_activity_database() {
    let state = TestState::new();
    let args = vec![
        "--id".into(),
        "unknown-activity".into(),
        "--prompt".into(),
        "Inspect".into(),
        "--activity".into(),
        uuid::Uuid::new_v4().to_string(),
    ];
    assert!(preflight_activity_command(state.uid, "add", &args)
        .unwrap_err()
        .contains("not found"));
    assert!(!crate::paths::data_dir().exists());
    assert!(!triggers_dir().exists());
    assert!(!crate::paths::caps_audit_log_path().exists());
    state.assert_no_work();
}

#[test]
fn preflight_rejects_unbounded_stored_associations_without_recording_a_failure() {
    let state = TestState::new();
    let activity = state.activity(false);
    let rule = state.add("unbounded-preflight", None);
    update_rule(&rule.id, |mut current| {
        current.activity_id = Some(activity.id.clone());
        Ok(current)
    })
    .unwrap();
    let before = fs::read(rule_path(&rule.id)).unwrap();
    let audit = fs::read(crate::paths::caps_audit_log_path()).unwrap();
    for command in ["enable", "run"] {
        assert!(
            preflight_activity_command(state.uid, command, &["--id".into(), rule.id.clone()])
                .unwrap_err()
                .contains("configured finite")
        );
    }
    assert_eq!(fs::read(rule_path(&rule.id)).unwrap(), before);
    assert_eq!(
        fs::read(crate::paths::caps_audit_log_path()).unwrap(),
        audit
    );
    assert!(load_rule(&rule.id).unwrap().last_delivery.is_none());
    assert!(!cursor_path().exists());
    state.assert_no_work();
}

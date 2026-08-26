use super::*;

#[test]
fn status_active_flags() {
    assert!(Status::Pending.is_active());
    assert!(Status::Running.is_active());
    assert!(Status::Paused.is_active());
    assert!(!Status::Done.is_active());
    assert!(!Status::Failed.is_active());
}

#[test]
fn status_serializes_as_kebab() {
    assert_eq!(serde_json::to_string(&Status::Done).unwrap(), "\"done\"");
    assert_eq!(serde_json::to_string(&Status::Paused).unwrap(), "\"paused\"");
}

#[test]
fn meta_round_trip_default() {
    let m = SessionMeta::fresh(SessionId::generate(), "test purpose");
    let json = serde_json::to_string(&m).unwrap();
    let back: SessionMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
    // Default budget round-trips empty.
    assert!(back.budget.tokens.is_none());
    assert!(back.budget.wall_seconds.is_none());
    assert!(back.budget.mutations.is_none());
}

#[test]
fn meta_round_trip_full() {
    let m = SessionMeta {
        id: SessionId::generate(),
        purpose: "整理发票".into(),
        role: Some(Role::Automator),
        credential_tier: Some(Role::Automator.credential_tier()),
        owner_uid: Some(1000),
        parent_session: Some(SessionId::generate()),
        status: Status::Running,
        budget: Budget {
            tokens: Some(100_000),
            wall_seconds: Some(3600),
            mutations: Some(500),
        },
        created_at: "2026-01-01T00:00:00Z".into(),
        ended_at: None,
        creator_runtime: Some("cos-agent".into()),
    };
    let json = serde_json::to_string_pretty(&m).unwrap();
    let back: SessionMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn lease_round_trip() {
    let l = Lease {
        pid: 1234,
        runtime: Some("cos-agent-chat".into()),
        started_at: "2026-01-01T00:00:00Z".into(),
        heartbeat_at: "2026-01-01T00:00:05Z".into(),
    };
    let json = serde_json::to_string(&l).unwrap();
    let back: Lease = serde_json::from_str(&json).unwrap();
    assert_eq!(l, back);
}

#[test]
fn budget_skips_none_fields_in_json() {
    let b = Budget {
        tokens: Some(100),
        wall_seconds: None,
        mutations: None,
    };
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains("tokens"));
    assert!(!json.contains("wall_seconds"));
    assert!(!json.contains("mutations"));
}

use super::*;

fn args(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parse_args_requires_session_id_or_last() {
    let err = parse_args(&args(&[])).unwrap_err();
    assert!(err.contains("missing session id"), "got {err}");
}

#[test]
fn parse_args_accepts_positional_session_id() {
    let o = parse_args(&args(&["sess-1"])).unwrap();
    assert_eq!(o.session_id.as_deref(), Some("sess-1"));
    assert!(!o.use_last);
    assert_eq!(o.limit, 1000);
}

#[test]
fn parse_args_accepts_last_flag() {
    let o = parse_args(&args(&["--last"])).unwrap();
    assert!(o.use_last);
    assert_eq!(o.session_id, None);
}

#[test]
fn parse_args_accepts_session_flag() {
    let o = parse_args(&args(&["--session", "sess-2"])).unwrap();
    assert_eq!(o.session_id.as_deref(), Some("sess-2"));
}

#[test]
fn parse_args_limit_validates_positive_int() {
    assert!(parse_args(&args(&["s", "--limit", "0"]))
        .unwrap_err()
        .contains("> 0"));
    assert!(parse_args(&args(&["s", "--limit", "abc"]))
        .unwrap_err()
        .contains("positive integer"));
    let o = parse_args(&args(&["s", "--limit", "50"])).unwrap();
    assert_eq!(o.limit, 50);
}

#[test]
fn parse_args_role_normalises_case_and_validates() {
    let o = parse_args(&args(&["s", "--role", "USER"])).unwrap();
    assert_eq!(o.role_filter.as_deref(), Some("user"));
    let err = parse_args(&args(&["s", "--role", "robot"])).unwrap_err();
    assert!(err.contains("invalid --role"), "got {err}");
}

#[test]
fn parse_args_rejects_unknown_flag() {
    let err = parse_args(&args(&["s", "--bogus"])).unwrap_err();
    assert!(err.contains("unknown flag"), "got {err}");
}

#[test]
fn replay_with_empty_session_returns_zero_messages() {
    let db = MemoryDb::open_in_memory().unwrap();
    let opts = ReplayOpts {
        session_id: Some("ghost".into()),
        limit: 10,
        ..Default::default()
    };
    let v = replay_with(&db, &opts).unwrap();
    assert_eq!(v["session_id"], json!("ghost"));
    assert_eq!(v["message_count"], json!(0));
    assert_eq!(v["title"], json!(null));
}

#[test]
fn replay_with_returns_messages_chronologically() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.record_message("s1", "user", "first").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.record_message("s1", "assistant", "second").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.record_message("s1", "user", "third").unwrap();
    let opts = ReplayOpts {
        session_id: Some("s1".into()),
        limit: 100,
        ..Default::default()
    };
    let v = replay_with(&db, &opts).unwrap();
    assert_eq!(v["message_count"], json!(3));
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["content"], json!("first"));
    assert_eq!(msgs[1]["content"], json!("second"));
    assert_eq!(msgs[2]["content"], json!("third"));
}

#[test]
fn replay_with_role_filter_keeps_only_matching() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.record_message("s1", "user", "u1").unwrap();
    db.record_message("s1", "assistant", "a1").unwrap();
    db.record_message("s1", "user", "u2").unwrap();
    let opts = ReplayOpts {
        session_id: Some("s1".into()),
        limit: 100,
        role_filter: Some("user".into()),
        ..Default::default()
    };
    let v = replay_with(&db, &opts).unwrap();
    assert_eq!(v["message_count"], json!(2));
    let msgs = v["messages"].as_array().unwrap();
    for m in msgs {
        assert_eq!(m["role"], json!("user"));
    }
}

#[test]
fn replay_with_limit_caps_results() {
    let db = MemoryDb::open_in_memory().unwrap();
    for i in 0..5 {
        db.record_message("s1", "user", &format!("m{i}")).unwrap();
    }
    let opts = ReplayOpts {
        session_id: Some("s1".into()),
        limit: 2,
        ..Default::default()
    };
    let v = replay_with(&db, &opts).unwrap();
    assert_eq!(v["message_count"], json!(2));
}

#[test]
fn replay_with_last_picks_most_recent_session() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.record_message("old-sess", "user", "ancient").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.record_message("new-sess", "user", "recent").unwrap();
    let opts = ReplayOpts {
        use_last: true,
        limit: 10,
        ..Default::default()
    };
    let v = replay_with(&db, &opts).unwrap();
    assert_eq!(v["session_id"], json!("new-sess"));
    assert_eq!(v["message_count"], json!(1));
}

#[test]
fn replay_with_last_errors_on_empty_db() {
    let db = MemoryDb::open_in_memory().unwrap();
    let opts = ReplayOpts {
        use_last: true,
        limit: 10,
        ..Default::default()
    };
    let err = replay_with(&db, &opts).unwrap_err();
    assert!(err.contains("no sessions"), "got {err}");
}

#[test]
fn replay_with_includes_title_when_set() {
    let db = MemoryDb::open_in_memory().unwrap();
    db.record_message("s1", "user", "hello").unwrap();
    db.set_title("s1", "Greeting Session").unwrap();
    let opts = ReplayOpts {
        session_id: Some("s1".into()),
        limit: 10,
        ..Default::default()
    };
    let v = replay_with(&db, &opts).unwrap();
    assert_eq!(v["title"], json!("Greeting Session"));
}

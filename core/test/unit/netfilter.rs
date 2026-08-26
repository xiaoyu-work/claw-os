use super::*;
use std::sync::{Mutex, Once};

static INIT: Once = Once::new();
/// Netfilter tests must be serialized because they all write to the same
/// rules.json file. Each test locks this mutex, resets rules, then runs.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the lock, init the shared dir once, then reset rules.
fn setup() -> std::sync::MutexGuard<'static, ()> {
    let guard = TEST_LOCK.lock().unwrap();
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("COS_DATA_DIR", &dir);
        // Tests don't set COS_SESSION; flip caps to permissive so the
        // gated dispatchers don't deny every call.
        std::env::set_var("COS_PERMS_MODE", "permissive");
    });
    std::env::remove_var("COS_SESSION");
    let _ = cmd_reset(&vec![]);
    guard
}

#[test]
fn domain_match_exact() {
    assert!(domain_matches("example.com", "example.com"));
    assert!(!domain_matches("example.com", "other.com"));
}

#[test]
fn domain_match_wildcard() {
    assert!(domain_matches("*.example.com", "sub.example.com"));
    assert!(domain_matches("*.example.com", "example.com"));
    assert!(!domain_matches("*.example.com", "other.com"));
}

#[test]
fn domain_match_star() {
    assert!(domain_matches("*", "anything.com"));
}

#[test]
fn add_and_list_rules() {
    let _g = setup();
    cmd_add(&vec!["--allow".into(), "github.com".into()]).unwrap();
    cmd_add(&vec!["--deny".into(), "evil.com".into()]).unwrap();

    let r = cmd_list(&vec![]).unwrap();
    assert_eq!(r["count"], 2);
}

#[test]
fn check_domain_with_rules() {
    let _g = setup();
    cmd_add(&vec!["--allow".into(), "api.openai.com".into()]).unwrap();
    cmd_add(&vec!["--deny".into(), "*.malware.com".into()]).unwrap();

    assert!(is_domain_allowed("api.openai.com"));
    assert!(!is_domain_allowed("sub.malware.com"));
}

#[test]
fn deny_all_default() {
    let _g = setup();
    cmd_default(&vec!["deny-all".into()]).unwrap();
    cmd_add(&vec!["--allow".into(), "github.com".into()]).unwrap();

    assert!(is_domain_allowed("github.com"));
    assert!(!is_domain_allowed("random.com"));
}

#[test]
fn remove_rule() {
    let _g = setup();
    cmd_add(&vec!["--allow".into(), "temp.com".into()]).unwrap();
    cmd_remove(&vec!["temp.com".into()]).unwrap();

    let r = cmd_list(&vec![]).unwrap();
    assert_eq!(r["count"], 0);
}

#[test]
fn reset_clears_all() {
    let _g = setup();
    cmd_add(&vec!["--allow".into(), "a.com".into()]).unwrap();
    cmd_add(&vec!["--deny".into(), "b.com".into()]).unwrap();
    cmd_reset(&vec![]).unwrap();

    let r = cmd_list(&vec![]).unwrap();
    assert_eq!(r["count"], 0);
    assert_eq!(r["default_policy"], "allow-all");
}

#[test]
fn run_dispatch() {
    let _g = setup();
    let r = run("add", &vec!["--allow".into(), "test.com".into()]).unwrap();
    assert_eq!(r["added"], true);

    let r = run("bogus", &vec![]);
    assert!(r.is_err());
}

// --- HTTP-level policy tests ---

#[test]
fn path_match_exact() {
    assert!(path_matches("/api/v1", "/api/v1"));
    assert!(!path_matches("/api/v1", "/api/v2"));
}

#[test]
fn path_match_single_wildcard() {
    assert!(path_matches("/api/*", "/api/users"));
    assert!(!path_matches("/api/*", "/api/users/123"));
}

#[test]
fn path_match_double_wildcard() {
    assert!(path_matches("/api/**", "/api/users"));
    assert!(path_matches("/api/**", "/api/users/123/posts"));
    assert!(path_matches("/**", "/anything/at/all"));
}

#[test]
fn evaluate_with_method_filter() {
    let _g = setup();
    cmd_default(&vec!["deny-all".into()]).unwrap();
    cmd_add(&vec![
        "--allow".into(),
        "api.example.com".into(),
        "--method".into(),
        "GET,POST".into(),
    ])
    .unwrap();

    let r = evaluate("api.example.com", Some("GET"), None, None);
    assert!(r.allowed);

    let r = evaluate("api.example.com", Some("DELETE"), None, None);
    assert!(!r.allowed);
}

#[test]
fn evaluate_with_path_filter() {
    let _g = setup();
    cmd_default(&vec!["deny-all".into()]).unwrap();
    cmd_add(&vec![
        "--allow".into(),
        "api.telegram.org".into(),
        "--path".into(),
        "/bot/**".into(),
    ])
    .unwrap();

    let r = evaluate("api.telegram.org", None, Some("/bot/sendMessage"), None);
    assert!(r.allowed);

    let r = evaluate("api.telegram.org", None, Some("/admin/delete"), None);
    assert!(!r.allowed);
}

#[test]
fn evaluate_with_binary_filter() {
    let _g = setup();
    cmd_default(&vec!["deny-all".into()]).unwrap();
    cmd_add(&vec![
        "--allow".into(),
        "github.com".into(),
        "--binary".into(),
        "/usr/bin/git".into(),
    ])
    .unwrap();

    let r = evaluate("github.com", None, None, Some("/usr/bin/git"));
    assert!(r.allowed);

    let r = evaluate("github.com", None, None, Some("/usr/bin/curl"));
    assert!(!r.allowed);
}

#[test]
fn evaluate_combined_filters() {
    let _g = setup();
    cmd_default(&vec!["deny-all".into()]).unwrap();
    cmd_add(&vec![
        "--allow".into(),
        "api.openai.com".into(),
        "--method".into(),
        "POST".into(),
        "--path".into(),
        "/v1/chat/**".into(),
    ])
    .unwrap();

    // POST to /v1/chat/completions — allowed
    let r = evaluate(
        "api.openai.com",
        Some("POST"),
        Some("/v1/chat/completions"),
        None,
    );
    assert!(r.allowed);

    // GET to /v1/chat/completions — denied (wrong method)
    let r = evaluate(
        "api.openai.com",
        Some("GET"),
        Some("/v1/chat/completions"),
        None,
    );
    assert!(!r.allowed);

    // POST to /v1/models — denied (wrong path)
    let r = evaluate("api.openai.com", Some("POST"), Some("/v1/models"), None);
    assert!(!r.allowed);
}

#[test]
fn export_returns_full_config() {
    let _g = setup();
    cmd_add(&vec![
        "--allow".into(),
        "example.com".into(),
        "--method".into(),
        "GET".into(),
        "--path".into(),
        "/api/**".into(),
        "--tls".into(),
    ])
    .unwrap();

    let r = cmd_export(&vec![]).unwrap();
    assert_eq!(r["rules"][0]["domain"], "example.com");
    assert_eq!(r["rules"][0]["methods"][0], "GET");
    assert_eq!(r["rules"][0]["path"], "/api/**");
    assert_eq!(r["rules"][0]["tls_required"], true);
}

// --- Rate limiting tests ---

#[test]
fn test_rate_limit_add_and_list() {
    let _g = setup();
    let r = cmd_rate_limit(&vec![
        "api.openai.com".into(),
        "--rpm".into(),
        "60".into(),
        "--burst".into(),
        "10".into(),
    ])
    .unwrap();
    assert_eq!(r["domain"], "api.openai.com");
    assert_eq!(r["rpm"], 60);
    assert_eq!(r["burst"], 10);

    let r = cmd_rate_limits(&vec![]).unwrap();
    assert_eq!(r["count"], 1);
    assert_eq!(r["rate_limits"][0]["domain"], "api.openai.com");
    assert_eq!(r["rate_limits"][0]["rpm"], 60);
    assert_eq!(r["rate_limits"][0]["burst"], 10);
}

#[test]
fn test_rate_limit_remove() {
    let _g = setup();
    cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "60".into()]).unwrap();

    let r = cmd_rate_limit_remove(&vec!["api.openai.com".into()]).unwrap();
    assert_eq!(r["removed"], 1);

    let r = cmd_rate_limits(&vec![]).unwrap();
    assert_eq!(r["count"], 0);
}

#[test]
fn test_rate_check_allowed() {
    let _g = setup();
    cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "10".into()]).unwrap();

    for i in 0..5 {
        let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(r["allowed"], true, "request {i} should be allowed");
        assert_eq!(r["requests_in_window"], i);
    }
}

#[test]
fn test_rate_check_denied() {
    let _g = setup();
    cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "3".into()]).unwrap();

    for _ in 0..3 {
        let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(r["allowed"], true);
    }

    // 4th request should be denied
    let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
    assert_eq!(r["allowed"], false);
    assert_eq!(r["remaining"], 0);
    assert!(r["retry_after_secs"].as_u64().unwrap() > 0);
}

#[test]
fn test_rate_check_dry_run() {
    let _g = setup();
    cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "10".into()]).unwrap();

    // Dry run should not record
    let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
    assert_eq!(r["allowed"], true);
    assert_eq!(r["requests_in_window"], 0);

    // Still 0 after dry run
    let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
    assert_eq!(r["requests_in_window"], 0);

    // Real request records it
    let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
    assert_eq!(r["allowed"], true);
    assert_eq!(r["requests_in_window"], 0); // was 0 before this request

    // Now there's 1
    let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
    assert_eq!(r["requests_in_window"], 1);
}

#[test]
fn test_rate_check_window_cleanup() {
    let _g = setup();
    cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "10".into()]).unwrap();

    // Manually inject old timestamps that are outside the 60s window
    let old_time = (chrono::Utc::now() - chrono::Duration::seconds(120))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut state = RateLimitState::default();
    state
        .requests
        .insert("api.openai.com".into(), vec![old_time; 5]);
    save_rate_state(&state);

    // Old timestamps should be pruned — count should be 0
    let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
    assert_eq!(r["allowed"], true);
    assert_eq!(r["requests_in_window"], 0);
}

#[test]
fn test_find_rate_limit_wildcard() {
    let _g = setup();
    cmd_rate_limit(&vec!["*.openai.com".into(), "--rpm".into(), "30".into()]).unwrap();

    // Wildcard should match subdomains
    let config = load_config();
    let rl = find_rate_limit(&config, "api.openai.com");
    assert!(rl.is_some());
    assert_eq!(rl.unwrap().rpm, 30);

    // Should also match the base domain
    let rl = find_rate_limit(&config, "openai.com");
    assert!(rl.is_some());
}

#[test]
fn test_rate_limit_burst() {
    let _g = setup();
    cmd_rate_limit(&vec![
        "api.openai.com".into(),
        "--rpm".into(),
        "2".into(),
        "--burst".into(),
        "1".into(),
    ])
    .unwrap();

    // rpm=2, burst=1 → total limit = 3
    for i in 0..3 {
        let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(
            r["allowed"], true,
            "request {i} should be allowed (limit=3)"
        );
    }

    // 4th request should be denied
    let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
    assert_eq!(r["allowed"], false);
    assert_eq!(r["limit"], 3);
}

/// Regression for the CRITICAL lost-update race that effectively
/// disabled the rate limiter under concurrency. N parallel
/// callers all under the limit at the same time must be
/// serialized: after spawning more callers than the limit, the
/// number that report `allowed=true` MUST equal exactly the
/// configured limit.
///
/// Before porting `cmd_rate_check` to `filelock::update_locked`,
/// every thread saw the same starting counter, every thread
/// decided "still under limit", and every thread's own +1 write
/// clobbered the others — final counter ended at +1 instead of
/// +N, and every caller got `allowed=true`.
#[test]
fn rate_check_no_lost_update() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrd};
    use std::sync::Arc;

    let _g = setup();
    let domain = "concurrent.example.com";
    let limit: u32 = 8;
    cmd_rate_limit(&vec![
        domain.into(),
        "--rpm".into(),
        limit.to_string(),
    ])
    .unwrap();

    let allowed = Arc::new(AtomicUsize::new(0));
    let denied = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];
    for _ in 0..32 {
        let a = allowed.clone();
        let d = denied.clone();
        let domain = domain.to_string();
        handles.push(std::thread::spawn(move || {
            let r = cmd_rate_check(&vec![domain]).unwrap();
            if r["allowed"] == true {
                a.fetch_add(1, AOrd::SeqCst);
            } else {
                d.fetch_add(1, AOrd::SeqCst);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let a = allowed.load(AOrd::SeqCst);
    let d = denied.load(AOrd::SeqCst);
    assert_eq!(
        a, limit as usize,
        "expected exactly {limit} allowed (rate limiter must serialize); got allowed={a} denied={d}"
    );
    assert_eq!(d, 32 - limit as usize, "remaining must be denied");
}

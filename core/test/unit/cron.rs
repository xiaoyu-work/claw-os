use super::*;

use std::sync::Once;
static PERMS_INIT: Once = Once::new();
fn perms_init() {
    PERMS_INIT.call_once(|| std::env::set_var("COS_PERMS_MODE", "permissive"));
}
use chrono::TimeZone;

// -- Cron expression matching --

#[test]
fn test_cron_matches_every_minute() {
    perms_init();
    let t = chrono::Utc
        .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
        .unwrap();
    assert!(cron_matches("* * * * *", &t));

    let t2 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    assert!(cron_matches("* * * * *", &t2));
}

#[test]
fn test_cron_matches_specific() {
    perms_init();
    let t = chrono::Utc
        .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
        .unwrap();
    assert!(cron_matches("30 14 * * *", &t));
    assert!(!cron_matches("31 14 * * *", &t));
    assert!(!cron_matches("30 15 * * *", &t));
}

#[test]
fn test_cron_matches_step() {
    perms_init();
    // */5 means 0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55
    for min in [0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55] {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
            .unwrap();
        assert!(cron_matches("*/5 * * * *", &t), "should match minute {min}");
    }
    for min in [1, 2, 3, 4, 6, 7, 8, 9, 11] {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
            .unwrap();
        assert!(
            !cron_matches("*/5 * * * *", &t),
            "should NOT match minute {min}"
        );
    }
}

#[test]
fn test_cron_matches_range() {
    perms_init();
    for min in 1..=5 {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
            .unwrap();
        assert!(cron_matches("1-5 * * * *", &t), "should match minute {min}");
    }
    let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
    assert!(!cron_matches("1-5 * * * *", &t));
    let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 6, 0).unwrap();
    assert!(!cron_matches("1-5 * * * *", &t));
}

#[test]
fn test_cron_matches_list() {
    perms_init();
    for min in [1, 15, 30] {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
            .unwrap();
        assert!(
            cron_matches("1,15,30 * * * *", &t),
            "should match minute {min}"
        );
    }
    let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 2, 0).unwrap();
    assert!(!cron_matches("1,15,30 * * * *", &t));
}

#[test]
fn test_field_matches_star() {
    perms_init();
    for v in 0..=59 {
        assert!(field_matches("*", v, 0, 59));
    }
}

#[test]
fn test_field_matches_step_with_range() {
    perms_init();
    // 1-10/3 matches 1, 4, 7, 10
    assert!(field_matches("1-10/3", 1, 0, 59));
    assert!(field_matches("1-10/3", 4, 0, 59));
    assert!(field_matches("1-10/3", 7, 0, 59));
    assert!(field_matches("1-10/3", 10, 0, 59));
    assert!(!field_matches("1-10/3", 2, 0, 59));
    assert!(!field_matches("1-10/3", 11, 0, 59));
}

#[test]
fn test_cron_invalid_fields() {
    perms_init();
    let t = chrono::Utc
        .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
        .unwrap();
    // Too few fields
    assert!(!cron_matches("* * *", &t));
    // Too many fields
    assert!(!cron_matches("* * * * * *", &t));
}

#[test]
fn test_cron_day_of_week() {
    perms_init();
    // 2026-03-25 is a Wednesday (day 3)
    let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
    assert!(cron_matches("0 12 * * 3", &t)); // Wednesday
    assert!(!cron_matches("0 12 * * 1", &t)); // Monday
}

/// VIXIE/POSIX cron DoM/DoW OR-semantics regression. When BOTH
/// day-of-month (field 3) AND day-of-week (field 5) are restricted
/// (non-`*`), the job must fire if EITHER matches. The previous
/// AND-based implementation silently desynchronised crontabs
/// migrated from vixie-cron / systemd-cron — e.g. `0 0 1 * 1`
/// (run at midnight on the 1st of the month OR every Monday)
/// used to require both, so it almost never fired.
#[test]
fn test_cron_vixie_dom_dow_or_semantics() {
    perms_init();

    // 2026-03-25 is a Wednesday (DoW=3), day-of-month=25.
    let wed_25 = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
    // 2026-03-23 is a Monday (DoW=1), day-of-month=23.
    let mon_23 = chrono::Utc.with_ymd_and_hms(2026, 3, 23, 12, 0, 0).unwrap();
    // 2026-03-01 is a Sunday (DoW=0), day-of-month=1.
    let sun_1 = chrono::Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();

    // Schedule: 12:00 on the 1st of the month OR every Monday.
    let schedule = "0 12 1 * 1";

    // Sunday the 1st: DoM matches → fire.
    assert!(
        cron_matches(schedule, &sun_1),
        "vixie OR: DoM=1 must fire on the 1st (sun 1)"
    );
    // Monday the 23rd: DoW matches → fire.
    assert!(
        cron_matches(schedule, &mon_23),
        "vixie OR: DoW=1 (Mon) must fire on Monday (mon 23)"
    );
    // Wednesday the 25th: neither matches → do not fire.
    assert!(
        !cron_matches(schedule, &wed_25),
        "vixie OR: must NOT fire on Wed 25 (neither DoM nor DoW matches)"
    );

    // When DoM is `*` and DoW is restricted, only DoW gates.
    assert!(cron_matches("0 12 * * 3", &wed_25));
    assert!(!cron_matches("0 12 * * 3", &mon_23));

    // When DoW is `*` and DoM is restricted, only DoM gates.
    assert!(cron_matches("0 12 25 * *", &wed_25));
    assert!(!cron_matches("0 12 25 * *", &mon_23));

    // When both are `*`, the day predicate is unconditionally true
    // (still subject to minute/hour/month).
    assert!(cron_matches("0 12 * * *", &wed_25));
    assert!(cron_matches("0 12 * * *", &mon_23));
}

// -- next_run_time --

#[test]
fn test_next_run_time_every_minute() {
    perms_init();
    let from = chrono::Utc
        .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
        .unwrap();
    let next = next_run_time("* * * * *", &from).unwrap();
    assert_eq!(
        next,
        chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 14, 31, 0)
            .unwrap()
    );
}

#[test]
fn test_next_run_time_specific() {
    perms_init();
    let from = chrono::Utc
        .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
        .unwrap();
    let next = next_run_time("0 15 * * *", &from).unwrap();
    assert_eq!(
        next,
        chrono::Utc.with_ymd_and_hms(2026, 3, 25, 15, 0, 0).unwrap()
    );
}

// -- overlap policy deserialization --

#[test]
fn test_overlap_policy_deserialization() {
    perms_init();
    let policies = [
        (r#""Skip""#, OverlapPolicy::Skip),
        (r#""Queue""#, OverlapPolicy::Queue),
        (r#""Kill""#, OverlapPolicy::Kill),
        (r#""Allow""#, OverlapPolicy::Allow),
    ];
    for (json_str, expected) in policies {
        let parsed: OverlapPolicy = serde_json::from_str(json_str)
            .unwrap_or_else(|e| panic!("failed to parse {json_str}: {e}"));
        assert_eq!(parsed, expected);
    }
}

#[test]
fn test_overlap_policy_default() {
    perms_init();
    let policy: OverlapPolicy = Default::default();
    assert_eq!(policy, OverlapPolicy::Skip);
}

// -- validate_id --

#[test]
fn test_validate_id_valid() {
    perms_init();
    assert!(validate_id("my-job").is_ok());
    assert!(validate_id("job_1").is_ok());
    assert!(validate_id("test123").is_ok());
}

#[test]
fn test_validate_id_invalid() {
    perms_init();
    assert!(validate_id("").is_err());
    assert!(validate_id("has space").is_err());
    assert!(validate_id("has/slash").is_err());
    assert!(validate_id("has.dot").is_err());
}

// -- tail_string --

#[test]
fn test_tail_string_short() {
    perms_init();
    let short = "hello world";
    assert_eq!(tail_string(short), "hello world");
}

#[test]
fn test_tail_string_long() {
    perms_init();
    let long = "x".repeat(4000);
    let tailed = tail_string(&long);
    assert!(tailed.len() <= TAIL_BYTES + 4); // +4 for "..."
    assert!(tailed.starts_with("..."));
}

// -- storage integration tests (use temp dir) --

use std::sync::Mutex;

static CRON_INIT: Once = Once::new();
static CRON_LOCK: Mutex<()> = Mutex::new(());

fn cron_setup() -> (
    std::sync::MutexGuard<'static, ()>,
    crate::test_env::TestSessionGuard,
) {
    let guard = CRON_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    CRON_INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("COS_DATA_DIR", &dir);
    });
    let session = crate::test_env::TestSessionGuard::admin(&crate::paths::data_dir());
    // Clean up jobs and logs between tests
    let jdir = jobs_dir();
    if jdir.is_dir() {
        let _ = fs::remove_dir_all(&jdir);
    }
    let ldir = logs_dir();
    if ldir.is_dir() {
        let _ = fs::remove_dir_all(&ldir);
    }
    (guard, session)
}

#[test]
fn test_add_and_list() {
    perms_init();
    let _g = cron_setup();

    let args = vec![
        "test-job".to_string(),
        "--schedule".to_string(),
        "*/5 * * * *".to_string(),
        "--command".to_string(),
        "echo hello".to_string(),
        "--description".to_string(),
        "A test job".to_string(),
    ];
    let result = cmd_add(&args).unwrap();
    assert_eq!(result["added"], "test-job");
    assert_eq!(result["schedule"], "*/5 * * * *");

    // List should show the job
    let list_result = cmd_list(&[]).unwrap();
    assert_eq!(list_result["count"], 1);
    let jobs = list_result["jobs"].as_array().unwrap();
    assert_eq!(jobs[0]["id"], "test-job");
    assert_eq!(jobs[0]["enabled"], true);
}

#[test]
fn test_add_duplicate() {
    perms_init();
    let _g = cron_setup();

    let args = vec![
        "dup-job".to_string(),
        "--schedule".to_string(),
        "* * * * *".to_string(),
        "--command".to_string(),
        "echo hi".to_string(),
    ];
    cmd_add(&args).unwrap();
    let err = cmd_add(&args).unwrap_err();
    assert!(err.contains("already exists"));
}

#[test]
fn test_remove() {
    perms_init();
    let _g = cron_setup();

    let args = vec![
        "rm-job".to_string(),
        "--schedule".to_string(),
        "* * * * *".to_string(),
        "--command".to_string(),
        "echo bye".to_string(),
    ];
    cmd_add(&args).unwrap();

    let result = cmd_remove(&["rm-job".to_string()]).unwrap();
    assert_eq!(result["removed"], "rm-job");

    // Should be gone from list
    let list_result = cmd_list(&[]).unwrap();
    assert_eq!(list_result["count"], 0);
}

#[test]
fn test_remove_nonexistent() {
    perms_init();
    let _g = cron_setup();
    let err = cmd_remove(&["no-such-job".to_string()]).unwrap_err();
    assert!(err.contains("not found"));
}

#[test]
fn test_enable_disable() {
    perms_init();
    let _g = cron_setup();

    let args = vec![
        "toggle-job".to_string(),
        "--schedule".to_string(),
        "0 * * * *".to_string(),
        "--command".to_string(),
        "echo toggle".to_string(),
    ];
    cmd_add(&args).unwrap();

    // Disable
    let result = cmd_disable(&["toggle-job".to_string()]).unwrap();
    assert_eq!(result["enabled"], false);

    // Verify via status
    let status = cmd_status(&["toggle-job".to_string()]).unwrap();
    assert_eq!(status["enabled"], false);
    assert_eq!(status["next_run"], Value::Null);

    // Enable
    let result = cmd_enable(&["toggle-job".to_string()]).unwrap();
    assert_eq!(result["enabled"], true);

    // Verify enabled and next_run is set
    let status = cmd_status(&["toggle-job".to_string()]).unwrap();
    assert_eq!(status["enabled"], true);
    assert!(status["next_run"].is_string());
}

#[test]
fn test_status() {
    perms_init();
    let _g = cron_setup();

    let args = vec![
        "status-job".to_string(),
        "--schedule".to_string(),
        "30 14 * * *".to_string(),
        "--command".to_string(),
        "echo status".to_string(),
        "--tier".to_string(),
        "1".to_string(),
        "--scope".to_string(),
        "/home/cos/project".to_string(),
        "--overlap".to_string(),
        "allow".to_string(),
        "--timeout".to_string(),
        "300".to_string(),
    ];
    cmd_add(&args).unwrap();

    let result = cmd_status(&["status-job".to_string()]).unwrap();
    assert_eq!(result["id"], "status-job");
    assert_eq!(result["schedule"], "30 14 * * *");
    assert_eq!(result["tier"], 1);
    assert_eq!(result["scope"], "/home/cos/project");
    assert_eq!(result["overlap_policy"], "Allow");
    assert_eq!(result["timeout_secs"], 300);
}

#[test]
fn test_run_dispatch() {
    perms_init();
    let _g = cron_setup();

    // Add a simple echo job
    let add_args = vec![
        "echo-job".to_string(),
        "--schedule".to_string(),
        "* * * * *".to_string(),
        "--command".to_string(),
        "echo cron-test-output".to_string(),
    ];
    cmd_add(&add_args).unwrap();

    // Run it manually
    let result = cmd_run(&["echo-job".to_string()]).unwrap();
    let status = result["status"].as_str().unwrap();
    // The command should succeed or fail (depends on shell availability)
    assert!(
        status == "success" || status == "failed",
        "unexpected status: {status}"
    );

    // Logs should have an entry
    let logs = cmd_logs(&["echo-job".to_string()]).unwrap();
    assert!(logs["count"].as_u64().unwrap() >= 1);
}

#[test]
fn test_logs_limit() {
    perms_init();
    let _g = cron_setup();

    // Create a job and save multiple log entries
    let args = vec![
        "log-job".to_string(),
        "--schedule".to_string(),
        "* * * * *".to_string(),
        "--command".to_string(),
        "echo hi".to_string(),
    ];
    cmd_add(&args).unwrap();

    // Save 5 fake log entries
    for i in 0..5 {
        let result = CronRunResult {
            started_at: format!("2026-03-25T10-{:02}-00Z", i),
            finished_at: Some(format!("2026-03-25T10-{:02}-01Z", i)),
            exit_code: Some(0),
            status: "success".to_string(),
            stdout_tail: Some(format!("output {i}")),
            stderr_tail: None,
            duration_ms: Some(100),
            run_id: None,
            pid: None,
            pid_start_time_ticks: None,
        };
        save_run_log("log-job", &result).unwrap();
    }

    // Default limit (20) should return all 5
    let logs = cmd_logs(&["log-job".to_string()]).unwrap();
    assert_eq!(logs["count"], 5);

    // Limit to 2
    let logs = cmd_logs(&[
        "log-job".to_string(),
        "--limit".to_string(),
        "2".to_string(),
    ])
    .unwrap();
    assert_eq!(logs["count"], 2);
}

#[test]
fn test_unknown_command() {
    perms_init();
    let err = run("nonexistent", &[]).unwrap_err();
    assert!(err.contains("unknown cron command"));
}

#[test]
fn test_add_missing_schedule() {
    perms_init();
    let _g = cron_setup();
    let args = vec![
        "bad-job".to_string(),
        "--command".to_string(),
        "echo hi".to_string(),
    ];
    let err = cmd_add(&args).unwrap_err();
    assert!(err.contains("--schedule"));
}

#[test]
fn test_add_missing_command() {
    perms_init();
    let _g = cron_setup();
    let args = vec![
        "bad-job".to_string(),
        "--schedule".to_string(),
        "* * * * *".to_string(),
    ];
    let err = cmd_add(&args).unwrap_err();
    assert!(err.contains("--command"));
}

#[test]
fn test_add_invalid_schedule() {
    perms_init();
    let _g = cron_setup();
    let args = vec![
        "bad-sched".to_string(),
        "--schedule".to_string(),
        "* *".to_string(),
        "--command".to_string(),
        "echo hi".to_string(),
    ];
    let err = cmd_add(&args).unwrap_err();
    assert!(err.contains("5 fields"));
}

#[test]
fn test_tick_no_jobs() {
    perms_init();
    let _g = cron_setup();
    let result = cmd_tick(&[]).unwrap();
    assert_eq!(result["processed"], 0);
}

#[test]
fn test_cronjob_serialization_roundtrip() {
    perms_init();
    let job = CronJob {
        id: "roundtrip".to_string(),
        schedule: "*/10 * * * *".to_string(),
        command: "echo hello".to_string(),
        description: "test roundtrip".to_string(),
        tier: Some(1),
        scope: Some("/home/cos".to_string()),
        credentials: vec!["key1".to_string(), "key2".to_string()],
        enabled: true,
        overlap_policy: OverlapPolicy::Queue,
        timeout_secs: Some(60),
        created_at: "2026-03-25T10:00:00Z".to_string(),
        last_run: None,
        next_run: Some("2026-03-25T10:10:00Z".to_string()),
        owner_uid: None,
        owner_home: None,
        caps: None,
        role: None,
    };

    let json = serde_json::to_string(&job).unwrap();
    let parsed: CronJob = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "roundtrip");
    assert_eq!(parsed.tier, Some(1));
    assert_eq!(parsed.credentials.len(), 2);
    assert_eq!(parsed.overlap_policy, OverlapPolicy::Queue);
    assert_eq!(parsed.timeout_secs, Some(60));
}

#[test]
fn test_is_running() {
    perms_init();
    let mut job = CronJob {
        id: "run-check".to_string(),
        schedule: "* * * * *".to_string(),
        command: "echo hi".to_string(),
        description: String::new(),
        tier: None,
        scope: None,
        credentials: Vec::new(),
        enabled: true,
        overlap_policy: OverlapPolicy::Skip,
        timeout_secs: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        last_run: None,
        next_run: None,
        owner_uid: None,
        owner_home: None,
        caps: None,
        role: None,
    };

    assert!(!is_running(&job));

    job.last_run = Some(CronRunResult {
        started_at: format_time(&chrono::Utc::now()),
        finished_at: None,
        exit_code: None,
        status: "running".to_string(),
        stdout_tail: None,
        stderr_tail: None,
        duration_ms: None,
        run_id: None,
        pid: None,
        pid_start_time_ticks: None,
    });
    assert!(is_running(&job));

    job.last_run = Some(CronRunResult {
        started_at: "2026-01-01T00:00:00Z".to_string(),
        finished_at: Some("2026-01-01T00:01:00Z".to_string()),
        exit_code: Some(0),
        status: "success".to_string(),
        stdout_tail: None,
        stderr_tail: None,
        duration_ms: Some(60000),
        run_id: None,
        pid: None,
        pid_start_time_ticks: None,
    });
    assert!(!is_running(&job));
}

// -- wait_with_timeout deadlock regression --

/// Spawn a child that produces 512 KiB of stdout (8x the kernel
/// pipe buffer on Linux) and finishes within ~1s. Wrap the call
/// in wait_with_timeout(timeout_secs=10). Before the drainer-
/// thread fix this hangs forever and returns status="timeout".
/// After the fix it returns status="success" with full stdout.
#[test]
fn wait_with_timeout_drains_large_stdout_no_false_timeout() {
    perms_init();
    use std::time::Duration;

    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg("head -c 524288 /dev/zero | tr '\\0' 'x'")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let started_at = format_time(&chrono::Utc::now());
    let start = chrono::Utc::now();

    // Use mpsc to enforce an OUTER deadline that catches a real
    // deadlock (the inner timeout_secs is generous on purpose:
    // if drainer threads work the job finishes in <1s).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let res = wait_with_timeout(child, &started_at, &start, 10);
        let _ = tx.send(res);
    });
    let res = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("wait_with_timeout deadlocked — drainer threads not in effect");

    assert_eq!(
        res.status, "success",
        "expected success, got status={} (would be 'timeout' before the fix)",
        res.status
    );
    assert_eq!(res.exit_code, Some(0));
    // tail_string truncates; we don't care about exact length,
    // only that we received SOMETHING (proves stdout was drained).
    assert!(
        res.stdout_tail.is_some(),
        "stdout_tail must be Some after draining 512 KiB"
    );
}

/// Companion: a child that ignores SIGTERM and writes to stdout
/// every 50ms must still be killed and reported as timeout
/// AFTER the timeout elapses, not deadlock the dispatcher.
/// We use timeout_secs=1 and an outer 8s deadline.
#[test]
fn wait_with_timeout_terminates_slow_child_and_reports_timeout() {
    perms_init();
    use std::time::Duration;

    let child = std::process::Command::new("sh")
        .arg("-c")
        // Print one line every 50ms forever. Will run until killed.
        .arg("while :; do echo tick; sleep 0.05; done")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let started_at = format_time(&chrono::Utc::now());
    let start = chrono::Utc::now();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let res = wait_with_timeout(child, &started_at, &start, 1);
        let _ = tx.send(res);
    });
    let res = rx
        .recv_timeout(Duration::from_secs(8))
        .expect("wait_with_timeout failed to kill on deadline");

    assert_eq!(res.status, "timeout");
    // Note about the kill MUST be appended to stderr_tail even
    // when stderr was empty pre-kill.
    let stderr_tail = res.stderr_tail.unwrap_or_default();
    assert!(
        stderr_tail.contains("killed: timeout after 1s"),
        "stderr_tail missing kill marker: {stderr_tail:?}"
    );
}

#[test]
fn crashed_running_recovered_on_startup() {
    // Reproduces the crashed-mid-run availability bug: a job
    // whose last_run was stamped `status: "running"` before cos
    // crashed must NOT be treated as still-running forever
    // under the default Skip policy. The fix records `pid` in
    // `last_run`; `is_running` checks whether that pid is
    // actually alive.
    perms_init();
    let _g = cron_setup();

    // 1) Add a job under Skip policy.
    cmd_add(&[
        "crash-job".to_string(),
        "--schedule".to_string(),
        "* * * * *".to_string(),
        "--command".to_string(),
        "true".to_string(),
        "--overlap-policy".to_string(),
        "skip".to_string(),
    ])
    .unwrap();

    // 2) Simulate the on-disk state of a crashed run: stamp
    //    last_run = {status: running, pid: definitely-dead}.
    //    Pid 1 is alive on basically every system, so pick a
    //    pid that's almost certainly not in use. We claim the
    //    largest legal pid value (kernel.pid_max defaults to
    //    4194304 on Linux) which is unlikely to be allocated.
    let dead_pid: u32 = 0x7FFF_FFFE;
    // Verify it really is dead. If it isn't (extraordinarily
    // unlikely), pick another in a tight loop.
    let mut pid = dead_pid;
    while pid_alive(pid) {
        pid -= 1;
        if pid < 1000 {
            panic!("could not find a dead pid for test");
        }
    }
    let mut job = load_job("crash-job").unwrap();
    job.last_run = Some(CronRunResult {
        started_at: format_time(&chrono::Utc::now()),
        finished_at: None,
        exit_code: None,
        status: "running".to_string(),
        stdout_tail: None,
        stderr_tail: None,
        duration_ms: None,
        run_id: None,
        pid: Some(pid),
        pid_start_time_ticks: None,
    });
    save_job(&job).unwrap();

    // 3) Pre-fix: this would return true (stuck "running"
    //    forever). With the fix, is_running consults pid_alive.
    let reloaded = load_job("crash-job").unwrap();
    assert!(
        !is_running(&reloaded),
        "is_running must treat dead-pid 'running' rows as not-running"
    );

    // 4) cmd_run with overlap=Skip should now proceed (not
    //    skip) and execute the command successfully.
    let result = cmd_run(&["crash-job".to_string()]).unwrap();
    assert_ne!(
        result["status"], "skipped",
        "cmd_run wrongly skipped a crashed-stuck job: {result:?}"
    );
}

use super::*;

#[test]
fn redact_replaces_known_secrets() {
    let v = redact_cmd(&["my key is sk-abcdef0123456789ABCDEF0123456789abcd ok".into()])
        .expect("redact ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("[REDACTED:"),
        "expected placeholder, got {out}"
    );
    assert!(!out.contains("sk-abcdef0123456789ABCDEF0123456789abcd"));
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn redact_unchanged_for_clean_input() {
    let v = redact_cmd(&["hello world, this is just text".into()]).expect("redact ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert_eq!(out, "hello world, this is just text");
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(false));
}

#[test]
fn redact_check_returns_detection_only() {
    let v = redact_cmd(&["--check".into(), "leaks AKIAIOSFODNN7EXAMPLE here".into()])
        .expect("check ok");
    assert_eq!(
        v.get("contains_secrets").and_then(|x| x.as_bool()),
        Some(true)
    );
    assert!(
        v.get("redacted").is_none(),
        "check mode should not include redacted"
    );
}

#[test]
fn redact_check_negative() {
    let v = redact_cmd(&["--check".into(), "innocent text".into()]).expect("check ok");
    assert_eq!(
        v.get("contains_secrets").and_then(|x| x.as_bool()),
        Some(false)
    );
}

#[test]
fn redact_strict_flag_propagates() {
    let v = redact_cmd(&["--strict".into(), "contact me at user@example.com".into()])
        .expect("strict redact");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("[REDACTED:email]"),
        "strict should redact emails: {out}"
    );
    assert_eq!(v.get("strict").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn redact_default_does_not_redact_email() {
    let v = redact_cmd(&["contact me at user@example.com".into()]).expect("default redact");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("user@example.com"),
        "default should keep email: {out}"
    );
}

#[test]
fn redact_from_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("sample.txt");
    std::fs::write(&p, "token=ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789").expect("write");
    let v =
        redact_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("file redact ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("[REDACTED:github_token]"),
        "expected github_token placeholder, got {out}"
    );
}

#[test]
fn redact_joins_multiple_positional_args() {
    // Without --, the dispatcher will tokenize on spaces; we
    // re-stitch them so `cos agent redact hello world` doesn't
    // error out.
    let v = redact_cmd(&[
        "hello".into(),
        "this".into(),
        "has".into(),
        "Bearer".into(),
        "abcdefABCDEF1234567890123456789012345678".into(),
    ])
    .expect("multi-arg ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(out.contains("[REDACTED:"), "got {out}");
}

// ---- tools_cmd ----

#[test]
fn tools_cmd_default_lists_permitted_tools() {
    let v = tools_cmd(&[]).expect("tools list ok");
    let arr = v
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    assert!(
        !arr.is_empty(),
        "default registry should have at least echo + now"
    );
    // Every entry should be permitted under the default permissive guardrails.
    for entry in arr {
        assert_eq!(entry.get("permitted"), Some(&serde_json::Value::Bool(true)));
    }
    let permitted_count = v
        .get("permitted_count")
        .and_then(|c| c.as_u64())
        .unwrap_or(0);
    assert_eq!(permitted_count as usize, arr.len());
}

#[test]
fn tools_cmd_show_returns_full_schema() {
    let v = tools_cmd(&["show".into(), "echo".into()]).expect("tools show ok");
    assert_eq!(v.get("name").and_then(|n| n.as_str()), Some("echo"));
    assert!(v.get("description").is_some());
    assert!(v.get("input_schema").is_some());
}

#[test]
fn tools_cmd_show_unknown_tool_errs() {
    let err = tools_cmd(&["show".into(), "does-not-exist".into()]).unwrap_err();
    assert!(err.contains("does-not-exist"));
}

#[test]
fn tools_cmd_llm_list_returns_serialised_tool_blob() {
    let v = tools_cmd(&["llm-list".into()]).expect("tools llm-list ok");
    let arr = v
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    assert!(!arr.is_empty());
    for entry in arr {
        assert!(entry.get("name").and_then(|n| n.as_str()).is_some());
        assert!(entry.get("input_schema").is_some());
    }
}

#[test]
fn tools_cmd_unfiltered_includes_at_least_as_many_as_filtered() {
    let plain = tools_cmd(&["list".into()]).expect("plain list ok");
    let unfiltered = tools_cmd(&["list".into(), "--unfiltered".into()]).expect("unfiltered ok");
    let plain_count = plain
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let unfiltered_count = unfiltered
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(unfiltered_count >= plain_count);
}

// ---- guardrails_cmd ----

#[test]
fn guardrails_cmd_default_show_reports_permissive_mode() {
    let v = guardrails_cmd(&[]).expect("guardrails show ok");
    // Default config has no tool_allow / empty tool_deny → permissive.
    let mode = v.get("mode").and_then(|m| m.as_str()).unwrap_or("");
    assert!(
        mode == "permissive" || mode == "allowlist",
        "mode {mode:?} should be permissive or allowlist"
    );
    assert!(v.get("deny_count").and_then(|c| c.as_u64()).is_some());
}

#[test]
fn guardrails_cmd_check_returns_decision_for_known_tool() {
    let v = guardrails_cmd(&["check".into(), "echo".into()]).expect("guardrails check ok");
    let decision = v.get("decision").and_then(|d| d.as_str()).unwrap_or("");
    assert!(decision == "allow" || decision == "deny");
    assert_eq!(v.get("tool").and_then(|t| t.as_str()), Some("echo"));
}

#[test]
fn guardrails_cmd_check_requires_tool_name() {
    let err = guardrails_cmd(&["check".into()]).unwrap_err();
    assert!(err.contains("check"));
}

// ---- approval_cmd ----

#[test]
fn approval_cmd_default_show_returns_three_sets() {
    let v = approval_cmd(&[]).expect("approval show ok");
    assert!(v.get("auto_approve").and_then(|a| a.as_array()).is_some());
    assert!(v.get("auto_deny").and_then(|a| a.as_array()).is_some());
    assert!(v.get("dangerous").and_then(|a| a.as_array()).is_some());
}

#[test]
fn approval_cmd_check_safe_tool_returns_approved() {
    // Default config has no dangerous_tools → every tool short-circuits to approved.
    let v = approval_cmd(&["check".into(), "echo".into()]).expect("approval check ok");
    assert_eq!(v.get("decision").and_then(|d| d.as_str()), Some("approved"));
    assert_eq!(
        v.get("would_short_circuit").and_then(|b| b.as_bool()),
        Some(true)
    );
}

#[test]
fn approval_cmd_check_requires_tool_name() {
    let err = approval_cmd(&["check".into()]).unwrap_err();
    assert!(err.contains("check"));
}

#[test]
fn approval_cmd_check_input_must_parse_as_json() {
    let err = approval_cmd(&[
        "check".into(),
        "echo".into(),
        "--input".into(),
        "not json".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--input"));
}

#[test]
fn binary_ext_default_lists_with_capped_limit() {
    let v = binary_ext_cmd(&[]).expect("default ok");
    let n = v.get("n").and_then(|n| n.as_u64()).expect("n");
    let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
    assert!(n <= 50, "default limit should cap at 50, got {n}");
    assert!(total >= n, "total ({total}) must be >= shown ({n})");
    assert!(v.get("extensions").is_some());
}

#[test]
fn binary_ext_no_limit_returns_all() {
    let v = binary_ext_cmd(&["list".into(), "--no-limit".into()]).expect("no-limit ok");
    let n = v.get("n").and_then(|n| n.as_u64()).expect("n");
    let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
    assert_eq!(n, total);
}

#[test]
fn binary_ext_list_limit_requires_int() {
    let err = binary_ext_cmd(&["list".into(), "--limit".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--limit"));
}

#[test]
fn binary_ext_extensions_returns_all_unbounded() {
    let v = binary_ext_cmd(&["extensions".into()]).expect("extensions ok");
    let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
    let len = v
        .get("extensions")
        .and_then(|a| a.as_array())
        .map(|a| a.len() as u64)
        .expect("extensions array");
    assert_eq!(total, len);
}

#[test]
fn binary_ext_check_recognises_path_with_known_ext() {
    let v = binary_ext_cmd(&["check".into(), "C:\\Users\\me\\image.PNG".into()]).expect("check ok");
    assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("path"));
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(v.get("extension").and_then(|s| s.as_str()), Some("png"));
}

#[test]
fn binary_ext_check_recognises_text_path_as_not_binary() {
    let v = binary_ext_cmd(&["check".into(), "/etc/passwd".into()]).expect("check ok");
    assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("path"));
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(false));
}

#[test]
fn binary_ext_check_extension_only_input_uses_extension_mode() {
    let v = binary_ext_cmd(&["check".into(), ".gguf".into()]).expect("check ok");
    assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("extension"));
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(v.get("extension").and_then(|s| s.as_str()), Some("gguf"));

    let v2 = binary_ext_cmd(&["check".into(), "exe".into()]).expect("check ok2");
    assert_eq!(v2.get("mode").and_then(|s| s.as_str()), Some("extension"));
    assert_eq!(v2.get("is_binary").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn binary_ext_check_unknown_extension_returns_false() {
    let v = binary_ext_cmd(&["check".into(), "logfile.unknown".into()]).expect("check ok");
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(false));
}

// ---- file-safety dispatch ----

#[test]
fn file_safety_check_rejects_multiple_paths() {
    let err = file_safety_cmd(&["check".into(), "a".into(), "b".into()]).unwrap_err();
    assert!(err.contains("single path"));
}

#[test]
fn file_safety_check_allows_normal_file() {
    let v = file_safety_cmd(&["check".into(), "/home/user/project/main.rs".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
    assert!(v.get("category").and_then(|c| c.as_str()).is_none());
}

#[test]
fn file_safety_check_denies_credential_dir() {
    let v = file_safety_cmd(&["check".into(), "/home/user/.ssh/id_rsa".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert_eq!(
        v.get("category").and_then(|c| c.as_str()),
        Some("credential")
    );
}

#[test]
fn file_safety_check_denies_dangerous_extension() {
    let v = file_safety_cmd(&["check".into(), "/tmp/payload.exe".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert_eq!(
        v.get("category").and_then(|c| c.as_str()),
        Some("dangerous_extension")
    );
}

#[test]
fn file_safety_check_caution_for_shell_script() {
    let v = file_safety_cmd(&["check".into(), "/home/user/run.sh".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("caution"));
}

#[test]
fn file_safety_batch_aggregates_summary() {
    let v = file_safety_cmd(&[
        "batch".into(),
        "/home/user/main.rs".into(),
        "/etc/passwd".into(),
        "/home/user/run.sh".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(3));
    let summary = v.get("summary").and_then(|x| x.as_object()).unwrap();
    assert_eq!(summary.get("allow").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(summary.get("caution").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(summary.get("deny").and_then(|n| n.as_u64()), Some(1));
}

#[test]
fn file_safety_batch_requires_at_least_one_path() {
    let err = file_safety_cmd(&["batch".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn file_safety_categories_lists_known_categories() {
    let v = file_safety_cmd(&["categories".into()]).expect("ok");
    let cats = v.get("categories").and_then(|c| c.as_array()).unwrap();
    let names: Vec<&str> = cats.iter().filter_map(|c| c.as_str()).collect();
    assert!(names.contains(&"dangerous_extension"));
    assert!(names.contains(&"credential"));
    assert!(names.contains(&"system_directory"));
    assert!(names.contains(&"vcs_internal"));
    let verdicts = v.get("verdicts").and_then(|x| x.as_array()).unwrap();
    let vs: Vec<&str> = verdicts.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(vs, vec!["allow", "caution", "deny"]);
}

// ---- osv dispatch (no network) ----

#[test]
fn osv_ecosystems_lists_known_ecosystems() {
    let v = osv_cmd(&["ecosystems".into()]).expect("ok");
    let eco = v.get("ecosystems").and_then(|x| x.as_array()).unwrap();
    let names: Vec<&str> = eco.iter().filter_map(|s| s.as_str()).collect();
    assert!(names.contains(&"crates.io"));
    assert!(names.contains(&"npm"));
    assert!(names.contains(&"PyPI"));
    assert!(names.contains(&"Go"));
    let lockfiles = v.get("lockfiles").and_then(|x| x.as_array()).unwrap();
    let ls: Vec<&str> = lockfiles.iter().filter_map(|s| s.as_str()).collect();
    assert!(ls.contains(&"Cargo.lock"));
    assert!(ls.contains(&"go.sum"));
}

#[test]
fn osv_parse_rejects_extra_args() {
    let err = osv_cmd(&["parse".into(), "a".into(), "b".into()]).unwrap_err();
    assert!(err.contains("single"));
}

#[test]
fn osv_parse_reads_cargo_lock() {
    let dir = std::env::temp_dir().join(format!("cos-osv-parse-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("Cargo.lock");
    std::fs::write(
        &lock_path,
        "[[package]]\nname = \"foo\"\nversion = \"1.2.3\"\n\n[[package]]\nname = \"bar\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let v = osv_cmd(&["parse".into(), lock_path.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(2));
    let pkgs = v.get("packages").and_then(|x| x.as_array()).unwrap();
    let names: Vec<&str> = pkgs
        .iter()
        .filter_map(|p| p.get("name").and_then(|s| s.as_str()))
        .collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    for p in pkgs {
        assert_eq!(
            p.get("ecosystem").and_then(|s| s.as_str()),
            Some("crates.io")
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn osv_parse_unknown_lockfile_errs() {
    let dir = std::env::temp_dir().join(format!("cos-osv-bad-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("Pipfile.lock");
    std::fs::write(&p, "{}").unwrap();
    let err = osv_cmd(&["parse".into(), p.to_string_lossy().to_string()]).unwrap_err();
    assert!(err.contains("unknown lockfile"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn osv_query_requires_coord() {
    let err = osv_cmd(&["query".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn osv_query_requires_ecosystem_flag() {
    let err = osv_cmd(&["query".into(), "lodash@1.0.0".into()]).unwrap_err();
    assert!(err.contains("--ecosystem"));
}

#[test]
fn osv_query_rejects_malformed_coord() {
    let err = osv_cmd(&[
        "query".into(),
        "no-version".into(),
        "--ecosystem".into(),
        "npm".into(),
    ])
    .unwrap_err();
    assert!(err.contains("name>@<version"));
}

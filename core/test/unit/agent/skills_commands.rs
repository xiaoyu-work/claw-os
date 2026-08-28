use super::*;

#[test]
fn skills_root_returns_path() {
    let v = skills_cmd(&["root".into()]).expect("skills root ok");
    assert!(v.get("root").and_then(|x| x.as_str()).is_some());
    assert!(v.get("user_root").and_then(|x| x.as_str()).is_some());
    assert!(v.get("system_root").and_then(|x| x.as_str()).is_some());
}

#[test]
fn skills_list_shape_correct() {
    let v = skills_cmd(&[]).expect("skills list ok");
    assert!(v.get("loaded").is_some());
    assert!(v.get("disabled").is_some());
    assert!(v.get("errors").is_some());
    assert!(v.get("names").and_then(|x| x.as_array()).is_some());
    assert!(v.get("user_root").and_then(|x| x.as_str()).is_some());
    assert!(v.get("system_root").and_then(|x| x.as_str()).is_some());
}

#[test]
fn skills_info_unknown_id_errors() {
    let err = skills_cmd(&["info".into(), "definitely-not-a-real-skill".into()]).unwrap_err();
    assert!(err.contains("definitely-not-a-real-skill"));
}

#[test]
fn parse_owner_repo_accepts_valid_form() {
    let (o, r) = parse_owner_repo("clawos/skills-hub").unwrap();
    assert_eq!(o, "clawos");
    assert_eq!(r, "skills-hub");
}

#[test]
fn parse_owner_repo_trims_whitespace() {
    let (o, r) = parse_owner_repo(" foo / bar ").unwrap();
    assert_eq!(o, "foo");
    assert_eq!(r, "bar");
}

#[test]
fn parse_owner_repo_rejects_missing_slash() {
    let err = parse_owner_repo("noslashhere").unwrap_err();
    assert!(err.contains("owner"));
}

#[test]
fn parse_owner_repo_rejects_empty_segments() {
    assert!(parse_owner_repo("/repo").is_err());
    assert!(parse_owner_repo("owner/").is_err());
    assert!(parse_owner_repo("/").is_err());
    assert!(parse_owner_repo("").is_err());
}

#[test]
fn skills_usage_stats_empty_returns_zero_count() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let v = skills_usage_cmd_at(&["stats".into()], &p).expect("stats ok");
    assert_eq!(v.get("skill_count").and_then(|x| x.as_u64()), Some(0));
    assert_eq!(
        v.get("skills").and_then(|x| x.as_array()).map(|a| a.len()),
        Some(0)
    );
}

#[test]
fn skills_usage_record_then_stats_aggregates() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "100".into(),
            "--ok".into(),
        ],
        &p,
    )
    .expect("record 1");
    skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "200".into(),
            "--error".into(),
        ],
        &p,
    )
    .expect("record 2");
    let v = skills_usage_cmd_at(&["stats".into()], &p).expect("stats ok");
    let skills = v.get("skills").and_then(|x| x.as_array()).unwrap();
    assert_eq!(skills.len(), 1);
    let s = &skills[0];
    assert_eq!(s.get("id").and_then(|x| x.as_str()), Some("demo"));
    assert_eq!(s.get("total").and_then(|x| x.as_u64()), Some(2));
    assert_eq!(s.get("success").and_then(|x| x.as_u64()), Some(1));
    assert_eq!(s.get("failure").and_then(|x| x.as_u64()), Some(1));
    assert_eq!(
        s.get("total_duration_ms").and_then(|x| x.as_u64()),
        Some(300)
    );
    assert_eq!(
        s.get("average_duration_ms").and_then(|x| x.as_u64()),
        Some(150)
    );
}

#[test]
fn skills_usage_stats_filter_by_id() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    for id in ["a", "b", "c"] {
        skills_usage_cmd_at(
            &[
                "record".into(),
                id.into(),
                "--duration-ms".into(),
                "10".into(),
            ],
            &p,
        )
        .expect("rec");
    }
    let v = skills_usage_cmd_at(&["stats".into(), "b".into()], &p).expect("stats ok");
    let skills = v.get("skills").and_then(|x| x.as_array()).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(
        skills[0].get("id").and_then(|x| x.as_str()),
        Some("b"),
        "filter should keep only `b`"
    );
    assert_eq!(v.get("filter_id").and_then(|x| x.as_str()), Some("b"));
}

#[test]
fn skills_usage_record_requires_duration() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let err = skills_usage_cmd_at(&["record".into(), "demo".into()], &p).unwrap_err();
    assert!(err.contains("--duration-ms"));
}

#[test]
fn skills_usage_record_with_invoked_by_persists() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "5".into(),
            "--by".into(),
            "delegate".into(),
        ],
        &p,
    )
    .expect("record ok");
    let body = std::fs::read_to_string(&p).expect("read");
    assert!(body.contains("\"invoked_by\":\"delegate\""), "body: {body}");
}

#[test]
fn skills_usage_clear_refuses_without_yes() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    std::fs::write(&p, "junk").expect("write");
    let err = skills_usage_cmd_at(&["clear".into()], &p).unwrap_err();
    assert!(err.contains("--yes"));
    assert!(p.exists(), "file must remain after refused clear");
}

#[test]
fn skills_usage_clear_with_yes_removes_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    std::fs::write(&p, "junk").expect("write");
    let v = skills_usage_cmd_at(&["clear".into(), "--yes".into()], &p).expect("clear ok");
    assert_eq!(v.get("cleared").and_then(|x| x.as_bool()), Some(true));
    assert!(!p.exists(), "file should be removed");
}

#[test]
fn skills_usage_clear_missing_file_is_ok() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("does-not-exist.jsonl");
    let v = skills_usage_cmd_at(&["clear".into(), "--yes".into()], &p).expect("clear ok");
    assert_eq!(v.get("cleared").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn skills_usage_path_returns_path() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let v = skills_usage_cmd_at(&["path".into()], &p).expect("path ok");
    let returned = v.get("path").and_then(|x| x.as_str()).unwrap();
    assert!(returned.ends_with("usage.jsonl"), "got {returned}");
}

#[test]
fn skills_usage_record_unknown_flag_errors() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let err = skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "1".into(),
            "--bogus".into(),
        ],
        &p,
    )
    .unwrap_err();
    assert!(err.contains("--bogus"));
}

// ---- skills_guard_cmd ----

fn skills_guard_test_dir(label: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cos-agent-skills-guard-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_test_skill(
    dir: &std::path::Path,
    id: &str,
    tools: &[&str],
) -> crate::agent::skills::loader::LoadedSkill {
    use crate::agent::skills::loader::LoadedSkill;
    use std::fs;
    let sd = dir.join(id);
    fs::create_dir_all(&sd).unwrap();
    let mp = sd.join("SKILL.md");
    let allowed = if tools.is_empty() {
        String::new()
    } else {
        format!(
            "allowed-tools:\n{}\n",
            tools
                .iter()
                .map(|t| format!("  - {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    fs::write(
        &mp,
        format!("---\nname: {id}\ndescription: test\n{allowed}---\n# body\n"),
    )
    .unwrap();
    let doc = crate::agent::skills::manifest::parse(&fs::read_to_string(&mp).unwrap()).unwrap();
    LoadedSkill {
        id: id.to_string(),
        dir: sd,
        manifest_path: mp,
        manifest: doc.manifest,
        body_bytes: doc.body.len(),
        body: doc.body,
        origin: crate::agent::skills::loader::SkillOrigin::Local,
    }
}

fn guard_skills_map(
    skill: crate::agent::skills::loader::LoadedSkill,
) -> std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(skill.id.clone(), skill);
    m
}

#[test]
fn skills_guard_unknown_id_errs() {
    let map: std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> =
        std::collections::BTreeMap::new();
    let err = skills_guard_cmd_against(&["nope".into()], &map).unwrap_err();
    assert!(err.contains("not loaded"));
}

#[test]
fn skills_guard_missing_id_errs() {
    let map: std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> =
        std::collections::BTreeMap::new();
    let err = skills_guard_cmd_against(&[], &map).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn skills_guard_default_provenance_hub_allows_clean_skill() {
    let dir = skills_guard_test_dir("default-hub");
    let skill = write_test_skill(&dir, "alpha", &["echo"]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(&["alpha".into()], &map).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
    assert_eq!(v.get("provenance").and_then(|s| s.as_str()), Some("hub"));
}

#[test]
fn skills_guard_vendor_provenance_is_trusted() {
    // Even with require_allowed_tools + zero declared tools,
    // vendor provenance + honour_provenance_trust = Allow.
    let dir = skills_guard_test_dir("vendor-trust");
    let skill = write_test_skill(&dir, "beta", &[]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(
        &[
            "beta".into(),
            "--provenance".into(),
            "vendor".into(),
            "--require-allowed-tools".into(),
        ],
        &map,
    )
    .expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
}

#[test]
fn skills_guard_require_allowed_tools_denies_empty_hub_skill() {
    let dir = skills_guard_test_dir("require-tools");
    let skill = write_test_skill(&dir, "gamma", &[]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(&["gamma".into(), "--require-allowed-tools".into()], &map)
        .expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert!(v.get("reason").and_then(|s| s.as_str()).is_some());
}

#[test]
fn skills_guard_ignore_trust_strips_vendor_pass() {
    // vendor + ignore-trust + require-allowed-tools (empty) → deny.
    let dir = skills_guard_test_dir("ignore-trust");
    let skill = write_test_skill(&dir, "delta", &[]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(
        &[
            "delta".into(),
            "--provenance".into(),
            "vendor".into(),
            "--ignore-trust".into(),
            "--require-allowed-tools".into(),
        ],
        &map,
    )
    .expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert_eq!(
        v.get("config")
            .and_then(|c| c.get("honour_provenance_trust"))
            .and_then(|b| b.as_bool()),
        Some(false)
    );
}

#[test]
fn skills_guard_max_file_bytes_triggers_confirmation() {
    // Write a sibling file larger than the cap and verify the
    // verdict flips to require_confirmation.
    let dir = skills_guard_test_dir("max-bytes");
    let skill = write_test_skill(&dir, "epsilon", &["echo"]);
    // 200 bytes payload, cap = 100 bytes.
    std::fs::write(skill.dir.join("data.bin"), vec![0u8; 200]).unwrap();
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(
        &["epsilon".into(), "--max-file-bytes".into(), "100".into()],
        &map,
    )
    .expect("ok");
    assert_eq!(
        v.get("verdict").and_then(|s| s.as_str()),
        Some("require_confirmation")
    );
    assert!(v
        .get("reason")
        .and_then(|s| s.as_str())
        .map(|r| r.contains("data.bin"))
        .unwrap_or(false));
}

#[test]
fn skills_guard_unknown_provenance_errs() {
    let dir = skills_guard_test_dir("bad-prov");
    let skill = write_test_skill(&dir, "zeta", &["echo"]);
    let map = guard_skills_map(skill);
    let err = skills_guard_cmd_against(
        &["zeta".into(), "--provenance".into(), "alien".into()],
        &map,
    )
    .unwrap_err();
    assert!(err.contains("alien"));
}

#[test]
fn skills_guard_invalid_max_file_bytes_errs() {
    let dir = skills_guard_test_dir("bad-bytes");
    let skill = write_test_skill(&dir, "theta", &["echo"]);
    let map = guard_skills_map(skill);
    let err = skills_guard_cmd_against(
        &["theta".into(), "--max-file-bytes".into(), "lots".into()],
        &map,
    )
    .unwrap_err();
    assert!(err.contains("--max-file-bytes"));
}

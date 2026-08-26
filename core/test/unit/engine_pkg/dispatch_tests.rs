// End-to-end coverage for the consolidated CLI surface (5 commands).
// Earlier cuts had `info`/`install`/`pin`/`gc`/`uninstall`/`rollback` as
// separate dispatch arms; these tests pin the new shape so we don't
// accidentally re-grow them.

// End-to-end coverage for the consolidated CLI surface (5 commands).
// Earlier cuts had `info`/`install`/`pin`/`gc`/`uninstall`/`rollback` as
// separate dispatch arms; these tests pin the new shape so we don't
// accidentally re-grow them.

use super::*;

fn fresh_engines_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
    tmp
}

fn write_three_versions(engines_dir: &std::path::Path) {
    let json = serde_json::json!({
        "version": 1,
        "engines": {
            "llama-cpp": {
                "active": "v3",
                "previous": "v2",
                "installed": [
                    {"version": "v1", "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""},
                    {"version": "v2", "installed_at": "2026-02-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""},
                    {"version": "v3", "installed_at": "2026-03-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}
                ],
                "pinned": false,
                "channel": "release",
                "accelerator": "",
                "source": ""
            }
        }
    });
    std::fs::write(
        engines_dir.join("engines.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
    for v in ["v1", "v2", "v3"] {
        std::fs::create_dir_all(engines_dir.join("llama-cpp").join(v).join("lib")).unwrap();
    }
}

#[test]
fn rejected_legacy_subcommands_have_consistent_error_shape() {
    // Each of these used to be a top-level command. Make sure the
    // unknown-command error names them in the suggested set.
    for cmd in ["info", "install", "pin", "gc", "uninstall", "rollback"] {
        let err = run(cmd, &[]).expect_err("legacy command should be rejected");
        assert!(
            err.contains("unknown engine command")
                && err.contains("list | update | activate | remove | unpin"),
            "cmd={cmd}: unexpected error: {err}"
        );
    }
}

#[test]
fn list_without_args_returns_index_summary() {
    let _tmp = fresh_engines_dir();
    let v = run("list", &[]).expect("list should succeed");
    let obj = v.as_object().expect("object");
    assert!(obj.contains_key("engines"));
    assert!(obj.contains_key("engines_dir"));
    paths::set_engines_dir_override(None);
}

#[test]
fn list_with_engine_name_returns_detail_view() {
    let tmp = fresh_engines_dir();
    write_three_versions(tmp.path());
    let v = run("list", &["llama-cpp".to_string()]).expect("verbose list should succeed");
    let obj = v.as_object().expect("object");
    assert_eq!(
        obj.get("active").and_then(|v| v.as_str()),
        Some("v3"),
        "active should be the v3 we wrote",
    );
    assert!(
        obj.contains_key("active_manifest"),
        "detail view must attach active_manifest",
    );
    paths::set_engines_dir_override(None);
}

#[test]
fn list_verbose_alone_without_engine_is_explicit_error() {
    let _tmp = fresh_engines_dir();
    let err = run("list", &["--verbose".to_string()])
        .expect_err("--verbose with no name should error");
    assert!(err.contains("--verbose alone needs an engine name"));
    paths::set_engines_dir_override(None);
}

/// `remove <engine>@<version>` walks the uninstall path, not the gc path.
#[test]
fn remove_with_version_is_uninstall() {
    let tmp = fresh_engines_dir();
    write_three_versions(tmp.path());
    // v1 is neither active nor previous → safe to uninstall.
    let v = run("remove", &["llama-cpp@v1".to_string()]).expect("uninstall ok");
    assert_eq!(
        v.get("status").and_then(|v| v.as_str()),
        Some("uninstalled")
    );
    // Directory should be gone.
    assert!(!tmp.path().join("llama-cpp/v1").exists());
    paths::set_engines_dir_override(None);
}

/// `remove <engine>` (no version) walks the gc path.
#[test]
fn remove_without_version_is_gc() {
    let tmp = fresh_engines_dir();
    write_three_versions(tmp.path());
    // keep=1 means "keep one most-recent installed plus active+previous".
    let v = run(
        "remove",
        &[
            "llama-cpp".to_string(),
            "--keep".to_string(),
            "1".to_string(),
        ],
    )
    .expect("gc ok");
    assert_eq!(
        v.get("status").and_then(|v| v.as_str()),
        Some("gc-complete")
    );
    assert_eq!(v.get("kept").and_then(|v| v.as_u64()), Some(1));
    paths::set_engines_dir_override(None);
}

/// `remove <engine>@<ver> --keep N` is ambiguous on purpose.
#[test]
fn remove_versioned_with_keep_flag_is_rejected() {
    let _tmp = fresh_engines_dir();
    let err = run(
        "remove",
        &[
            "llama-cpp@v1".to_string(),
            "--keep".to_string(),
            "3".to_string(),
        ],
    )
    .expect_err("should reject ambiguous combo");
    assert!(err.contains("--keep is for gc-mode"));
    paths::set_engines_dir_override(None);
}

/// Update with `--from` rejects online-only flags up front so users
/// get a deterministic error instead of a partial offline install.
#[test]
fn update_from_archive_rejects_online_flags() {
    let _tmp = fresh_engines_dir();
    let err = run(
        "update",
        &[
            "llama-cpp".to_string(),
            "--from".to_string(),
            "/tmp/x.tar.gz".to_string(),
            "--to".to_string(),
            "b9999".to_string(),
        ],
    )
    .expect_err("--from + --to should be rejected");
    assert!(err.contains("offline") && err.contains("online-only"));
    paths::set_engines_dir_override(None);
}

/// Online --version is the wrong knob; we steer users to --to.
#[test]
fn update_online_with_version_flag_steers_to_tag_flag() {
    let _tmp = fresh_engines_dir();
    // No --from, and a stray --version: should suggest --to instead.
    let err = run(
        "update",
        &[
            "llama-cpp".to_string(),
            "--version".to_string(),
            "b4001".to_string(),
        ],
    )
    .expect_err("online --version should error");
    assert!(err.contains("--to"));
    paths::set_engines_dir_override(None);
}

#[test]
fn ort_genai_update_defaults_to_known_good_tag() {
    assert_eq!(
        online_release_tag("ort-genai", None),
        Some(ORT_GENAI_KNOWN_GOOD_TAG.to_string())
    );
    assert_eq!(
        online_release_tag("ort-genai", Some("v0.13.1".to_string())),
        Some("v0.13.1".to_string())
    );
    assert_eq!(online_release_tag("ort", None), None);
}

#[test]
fn unpin_clears_the_pin_flag() {
    let tmp = fresh_engines_dir();
    write_three_versions(tmp.path());
    // Set the pin via the registry directly (not exposed as a CLI in
    // the new surface — `update --pin` is the entry point).
    let mut idx = registry::EnginesIndex::load_or_default().expect("index loads");
    idx.set_pinned("llama-cpp", true).expect("set pin");
    idx.save().expect("save");

    let v = run("unpin", &["llama-cpp".to_string()]).expect("unpin ok");
    assert_eq!(v.get("status").and_then(|v| v.as_str()), Some("unpinned"));

    let idx = registry::EnginesIndex::load_or_default().expect("reload");
    assert!(!idx.entry("llama-cpp").unwrap().pinned);
    paths::set_engines_dir_override(None);
}

// ---------- digest-required guard (audit fix) ----------

/// Audit fix (engine_pkg HIGH "digest required"): online install
/// of a release asset that does NOT publish a SHA-256 digest
/// must be refused, because the kernel would otherwise execute
/// unverified native code shipped by the publisher's CDN. The
/// `COS_ENGINE_TRUST_UNVERIFIED=1` env var is the documented
/// emergency override.
///
/// We test the decision helper directly — the rest of the
/// install path requires a live network, which we don't want
/// in unit tests.
#[test]
fn digest_required() {
    // Clean env to a known state. Use a unique key with
    // env::remove because cargo test runs in a shared process.
    std::env::remove_var("COS_ENGINE_TRUST_UNVERIFIED");

    // No digest, no override → refuse with a clear message.
    let err = check_digest_requirement("llama-cpp", "b4001", "llama-cpp.tar.gz", None)
        .expect_err("must refuse without digest");
    assert!(
        err.contains("missing a SHA-256 digest"),
        "error must explain the refusal reason, got: {err}",
    );
    assert!(
        err.contains("COS_ENGINE_TRUST_UNVERIFIED"),
        "error must mention the override env var, got: {err}",
    );

    // Override → allowed.
    std::env::set_var("COS_ENGINE_TRUST_UNVERIFIED", "1");
    assert!(check_digest_requirement(
        "llama-cpp",
        "b4001",
        "llama-cpp.tar.gz",
        None
    )
    .is_ok());
    std::env::remove_var("COS_ENGINE_TRUST_UNVERIFIED");

    // Empty / "0" must NOT be treated as on.
    std::env::set_var("COS_ENGINE_TRUST_UNVERIFIED", "0");
    assert!(check_digest_requirement(
        "llama-cpp",
        "b4001",
        "llama-cpp.tar.gz",
        None
    )
    .is_err());
    std::env::set_var("COS_ENGINE_TRUST_UNVERIFIED", "");
    assert!(check_digest_requirement(
        "llama-cpp",
        "b4001",
        "llama-cpp.tar.gz",
        None
    )
    .is_err());
    std::env::remove_var("COS_ENGINE_TRUST_UNVERIFIED");

    // With a digest, the check passes regardless of env state.
    assert!(check_digest_requirement(
        "llama-cpp",
        "b4001",
        "llama-cpp.tar.gz",
        Some("deadbeef"),
    )
    .is_ok());
}

use super::*;

use std::sync::Once;
static PERMS_INIT: Once = Once::new();
fn perms_init() {
    PERMS_INIT.call_once(|| std::env::set_var("COS_PERMS_MODE", "permissive"));
}
use std::fs;

// -- Checkpoint ID generation --

#[test]
fn next_id_empty_dir() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-test-empty");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    assert_eq!(next_checkpoint_id(&dir), "001");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn next_id_sequential() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-test-seq");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    fs::create_dir_all(dir.join("001-first")).unwrap();
    fs::create_dir_all(dir.join("002-second")).unwrap();

    assert_eq!(next_checkpoint_id(&dir), "003");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn next_id_with_gap() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-test-gap");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    fs::create_dir_all(dir.join("001-alpha")).unwrap();
    fs::create_dir_all(dir.join("005-beta")).unwrap();

    // Should be max + 1, not fill gaps.
    assert_eq!(next_checkpoint_id(&dir), "006");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn next_id_ignores_non_numeric() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-test-nonnum");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    fs::create_dir_all(dir.join("not-a-number")).unwrap();
    fs::create_dir_all(dir.join("003-valid")).unwrap();

    assert_eq!(next_checkpoint_id(&dir), "004");

    let _ = fs::remove_dir_all(&dir);
}

// -- Meta serialization --

#[test]
fn meta_round_trip() {
    perms_init();
    let meta = CheckpointMeta {
        id: "007".to_string(),
        description: "before refactoring".to_string(),
        created_at: "2026-03-23T21:45:00Z".to_string(),
        files_changed: 15,
    };

    let json_str = serde_json::to_string_pretty(&meta).unwrap();
    let parsed: CheckpointMeta = serde_json::from_str(&json_str).unwrap();

    assert_eq!(parsed.id, "007");
    assert_eq!(parsed.description, "before refactoring");
    assert_eq!(parsed.created_at, "2026-03-23T21:45:00Z");
    assert_eq!(parsed.files_changed, 15);
}

#[test]
fn meta_json_has_expected_fields() {
    perms_init();
    let meta = CheckpointMeta {
        id: "001".to_string(),
        description: "test".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        files_changed: 3,
    };

    let v: Value = serde_json::to_value(&meta).unwrap();
    assert!(v["id"].is_string());
    assert!(v["description"].is_string());
    assert!(v["created_at"].is_string());
    assert!(v["files_changed"].is_number());
}

// -- walk_upper categorisation --

#[test]
fn walk_upper_created_files() {
    perms_init();
    let root = std::env::temp_dir().join("cos-cp-walk-created");
    let _ = fs::remove_dir_all(&root);

    let base_layer = root.join("base");
    let upper = root.join("upper");
    fs::create_dir_all(&base_layer).unwrap();
    fs::create_dir_all(&upper).unwrap();

    // File exists in upper but NOT in base → created.
    fs::write(upper.join("new.txt"), "hello").unwrap();

    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    walk_upper(
        &upper,
        &upper,
        &base_layer,
        &mut created,
        &mut modified,
        &mut deleted,
    )
    .unwrap();

    assert_eq!(created, vec!["new.txt"]);
    assert!(modified.is_empty());
    assert!(deleted.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn walk_upper_modified_files() {
    perms_init();
    let root = std::env::temp_dir().join("cos-cp-walk-modified");
    let _ = fs::remove_dir_all(&root);

    let base_layer = root.join("base");
    let upper = root.join("upper");
    fs::create_dir_all(&base_layer).unwrap();
    fs::create_dir_all(&upper).unwrap();

    // File exists in both base AND upper → modified.
    fs::write(base_layer.join("existing.txt"), "original").unwrap();
    fs::write(upper.join("existing.txt"), "changed").unwrap();

    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    walk_upper(
        &upper,
        &upper,
        &base_layer,
        &mut created,
        &mut modified,
        &mut deleted,
    )
    .unwrap();

    assert!(created.is_empty());
    assert_eq!(modified, vec!["existing.txt"]);
    assert!(deleted.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn walk_upper_subdirectory() {
    perms_init();
    let root = std::env::temp_dir().join("cos-cp-walk-subdir");
    let _ = fs::remove_dir_all(&root);

    let base_layer = root.join("base");
    let upper = root.join("upper");
    fs::create_dir_all(base_layer.join("src")).unwrap();
    fs::create_dir_all(upper.join("src")).unwrap();

    // Nested file: exists only in upper → created.
    fs::write(upper.join("src").join("lib.rs"), "fn main(){}").unwrap();

    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    walk_upper(
        &upper,
        &upper,
        &base_layer,
        &mut created,
        &mut modified,
        &mut deleted,
    )
    .unwrap();

    // Path separator may vary; just check the file name is present.
    assert_eq!(created.len(), 1);
    assert!(created[0].contains("lib.rs"));
    assert!(modified.is_empty());
    assert!(deleted.is_empty());

    let _ = fs::remove_dir_all(&root);
}

// -- sanitize_description --

#[test]
fn sanitize_basic() {
    perms_init();
    assert_eq!(
        sanitize_description("before refactoring"),
        "before-refactoring"
    );
}

#[test]
fn sanitize_special_chars() {
    perms_init();
    assert_eq!(
        sanitize_description("fix: tests & lints!"),
        "fix-tests-lints"
    );
}

#[test]
fn sanitize_empty() {
    perms_init();
    assert_eq!(sanitize_description(""), "");
}

// -- count_files_in_upper --

#[test]
fn count_files_empty() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-count-empty");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    assert_eq!(count_files_in_upper(&dir), 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn count_files_with_content() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-count-files");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub")).unwrap();

    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("sub").join("b.txt"), "b").unwrap();

    assert_eq!(count_files_in_upper(&dir), 2);

    let _ = fs::remove_dir_all(&dir);
}

// -- dir_size --

#[test]
fn dir_size_basic() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-dirsize");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    fs::write(dir.join("a.txt"), "hello").unwrap(); // 5 bytes

    let size = dir_size(&dir);
    assert!(size >= 5, "expected at least 5 bytes, got {size}");

    let _ = fs::remove_dir_all(&dir);
}

// -- copy_dir_recursive --

#[test]
fn copy_dir_recursive_works() {
    perms_init();
    let root = std::env::temp_dir().join("cos-cp-copydir");
    let _ = fs::remove_dir_all(&root);

    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), "aaa").unwrap();
    fs::write(src.join("sub").join("b.txt"), "bbb").unwrap();

    copy_dir_recursive(&src, &dst).unwrap();

    assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "aaa");
    assert_eq!(
        fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
        "bbb"
    );

    let _ = fs::remove_dir_all(&root);
}

/// Pre-fix: a symlink in the source tree was dereferenced —
/// `is_dir()` followed links and `fs::copy` materialized the
/// target's bytes. After: symlinks are preserved as symlinks
/// at the destination, with their original target path.
#[cfg(unix)]
#[test]
fn copy_dir_recursive_preserves_symlinks() {
    perms_init();
    let root = std::env::temp_dir().join("cos-cp-copydir-symlinks");
    let _ = fs::remove_dir_all(&root);

    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).unwrap();

    // file -> file symlink with a relative target
    fs::write(src.join("real.txt"), "real-bytes").unwrap();
    std::os::unix::fs::symlink("real.txt", src.join("link-to-file")).unwrap();

    // file -> file symlink with an absolute target that points
    // *outside* src; the pre-fix code would happily inline its
    // bytes, leaking external content into the checkpoint.
    let outside = root.join("outside.txt");
    fs::write(&outside, "outside-bytes").unwrap();
    std::os::unix::fs::symlink(&outside, src.join("link-absolute")).unwrap();

    // dir -> dir symlink — pre-fix this would have triggered
    // recursive copy of the linked directory contents.
    fs::create_dir_all(src.join("real_dir")).unwrap();
    fs::write(src.join("real_dir").join("inside.txt"), "inside-bytes").unwrap();
    std::os::unix::fs::symlink("real_dir", src.join("link-to-dir")).unwrap();

    // Dangling symlink — must still round-trip as a dangling
    // symlink rather than failing the copy.
    std::os::unix::fs::symlink("nonexistent-target", src.join("link-dangling")).unwrap();

    copy_dir_recursive(&src, &dst).unwrap();

    // The real file should be copied as a regular file.
    let real_meta = fs::symlink_metadata(dst.join("real.txt")).unwrap();
    assert!(real_meta.file_type().is_file(), "real.txt must stay a file");

    // All four links must be symlinks at dst (NOT regular files).
    for name in ["link-to-file", "link-absolute", "link-to-dir", "link-dangling"] {
        let meta = fs::symlink_metadata(dst.join(name))
            .unwrap_or_else(|e| panic!("symlink_metadata {name}: {e}"));
        assert!(
            meta.file_type().is_symlink(),
            "{name} must be a symlink at dst, not {:?}",
            meta.file_type()
        );
    }

    // Targets must round-trip unchanged.
    assert_eq!(fs::read_link(dst.join("link-to-file")).unwrap(), Path::new("real.txt"));
    assert_eq!(fs::read_link(dst.join("link-absolute")).unwrap(), outside);
    assert_eq!(fs::read_link(dst.join("link-to-dir")).unwrap(), Path::new("real_dir"));
    assert_eq!(
        fs::read_link(dst.join("link-dangling")).unwrap(),
        Path::new("nonexistent-target")
    );

    // real_dir was a real directory, so it should still be a
    // real directory at dst with its contents intact.
    let dir_meta = fs::symlink_metadata(dst.join("real_dir")).unwrap();
    assert!(
        dir_meta.file_type().is_dir() && !dir_meta.file_type().is_symlink(),
        "real_dir must stay a real directory"
    );
    assert_eq!(
        fs::read_to_string(dst.join("real_dir").join("inside.txt")).unwrap(),
        "inside-bytes"
    );

    let _ = fs::remove_dir_all(&root);
}

// -- existing_ids --

#[test]
fn existing_ids_empty() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-ids-empty");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    assert!(existing_ids(&dir).is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn existing_ids_mixed() {
    perms_init();
    let dir = std::env::temp_dir().join("cos-cp-ids-mixed");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    fs::create_dir_all(dir.join("002-foo")).unwrap();
    fs::create_dir_all(dir.join("010-bar")).unwrap();
    fs::create_dir_all(dir.join("readme")).unwrap(); // not numeric

    let mut ids = existing_ids(&dir);
    ids.sort();
    assert_eq!(ids, vec![2, 10]);

    let _ = fs::remove_dir_all(&dir);
}

// -- run dispatch --

#[test]
fn run_unknown_command() {
    perms_init();
    let result = run("bogus", &[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown checkpoint command"));
}

// -- parse_size --

#[test]
fn parse_size_gigabytes() {
    perms_init();
    assert_eq!(parse_size("2G").unwrap(), 2 * 1024 * 1024 * 1024);
}

#[test]
fn parse_size_megabytes() {
    perms_init();
    assert_eq!(parse_size("512M").unwrap(), 512 * 1024 * 1024);
}

#[test]
fn parse_size_kilobytes() {
    perms_init();
    assert_eq!(parse_size("100K").unwrap(), 100 * 1024);
}

#[test]
fn parse_size_bytes() {
    perms_init();
    assert_eq!(parse_size("1024").unwrap(), 1024);
}

#[test]
fn parse_size_invalid() {
    perms_init();
    assert!(parse_size("abc").is_err());
}

// -- format_bytes --

#[test]
fn format_bytes_gb() {
    perms_init();
    let s = format_bytes(2 * 1024 * 1024 * 1024);
    assert!(s.contains("G"));
}

#[test]
fn format_bytes_mb() {
    perms_init();
    let s = format_bytes(100 * 1024 * 1024);
    assert!(s.contains("M"));
}

// -- quota --

// Quota and namespace tests share a single COS_DATA_DIR (set via Once)
// and use a Mutex to serialize because they share global state (quota.json,
// namespace dirs).
use std::sync::Mutex;
static CP_INIT: Once = Once::new();
static CP_LOCK: Mutex<()> = Mutex::new(());

fn cp_setup() -> std::sync::MutexGuard<'static, ()> {
    let guard = CP_LOCK.lock().unwrap();
    CP_INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("COS_DATA_DIR", &dir);
    });
    std::env::remove_var("COS_SESSION");
    guard
}

#[test]
fn quota_set_and_status() {
    perms_init();
    let _g = cp_setup();

    let r = cmd_quota_set(&vec!["1G".into()]).unwrap();
    assert_eq!(r["quota_set"], true);

    let r = cmd_quota_status(&vec![]).unwrap();
    assert_eq!(r["quota_enabled"], true);
    assert_eq!(r["exceeded"], false);
}

// -- namespaces --

#[test]
fn namespace_create_list_destroy() {
    perms_init();
    let _g = cp_setup();
    let ns_name = format!("test-ns-{}", std::process::id());

    let r = create_namespace(&ns_name).unwrap();
    assert_eq!(r["created"], ns_name);

    let r = list_namespaces().unwrap();
    assert!(r["count"].as_u64().unwrap() >= 1);

    let r = namespace_status(&ns_name).unwrap();
    assert_eq!(r["namespace"], ns_name);
    assert_eq!(r["pending_changes"], 0);

    let r = destroy_namespace(&ns_name).unwrap();
    assert_eq!(r["destroyed"], ns_name);
}

#[test]
fn namespace_invalid_name() {
    perms_init();
    std::env::remove_var("COS_SESSION");
    let r = create_namespace("bad/name");
    assert!(r.is_err());
}

// -- rollback id validation --

/// Regression: rollback with an unknown id must reject the
/// command BEFORE wiping `upper/`. Pre-fix the function counted
/// pending changes, unmounted overlay, and removed upper/ at
/// step 2 — long before it tried to resolve the checkpoint id at
/// step 3. A user who typoed `cos checkpoint rollback abc`
/// destroyed all their uncommitted work and got an error.
#[test]
fn rollback_invalid_id_does_not_wipe_upper() {
    perms_init();
    let _g = cp_setup();

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let checkpoints = overlay.join("checkpoints");
    let _ = fs::remove_dir_all(&upper);
    let _ = fs::remove_dir_all(&checkpoints);
    fs::create_dir_all(&upper).unwrap();
    fs::create_dir_all(&checkpoints).unwrap();

    // Seed upper/ with a sentinel file that MUST survive the
    // failed rollback. This file represents the user's
    // uncommitted work.
    let sentinel = upper.join("uncommitted_work.txt");
    fs::write(&sentinel, b"do not destroy me").unwrap();

    // Seed at least one valid checkpoint so the checkpoints dir
    // isn't empty (catches a different code path).
    fs::create_dir_all(checkpoints.join("001-real/layer")).unwrap();

    // Attempt rollback with a bogus id. Must return Err.
    let res = cmd_rollback(&vec!["this-id-does-not-exist".to_string()]);
    assert!(res.is_err(), "expected Err for unknown checkpoint id, got {res:?}");

    // The sentinel MUST still exist — proof we validated before
    // touching upper/. Pre-fix this assertion would fail.
    assert!(
        sentinel.exists(),
        "upper/ was wiped despite invalid id; uncommitted work lost"
    );
    let body = fs::read_to_string(&sentinel).unwrap();
    assert_eq!(body, "do not destroy me");
}

/// Companion case: rollback with NO id is the explicit
/// "reset to base" command and IS allowed to wipe `upper/`.
/// This must still work after the validation hoist.
#[test]
fn rollback_no_id_resets_upper_to_base() {
    perms_init();
    let _g = cp_setup();

    let overlay = overlay_dir();
    let upper = overlay.join("upper");
    let checkpoints = overlay.join("checkpoints");
    let _ = fs::remove_dir_all(&upper);
    let _ = fs::remove_dir_all(&checkpoints);
    fs::create_dir_all(&upper).unwrap();
    fs::create_dir_all(&checkpoints).unwrap();

    fs::write(upper.join("scratch.txt"), b"x").unwrap();
    let res = cmd_rollback(&vec![]).unwrap();
    assert_eq!(res["rolled_back_to"], "base");
    assert!(upper.exists(), "upper should be re-created empty");
    assert!(!upper.join("scratch.txt").exists(), "scratch should be gone");
}

/// Crashes in the middle of cmd_create must NOT pollute the
/// checkpoint list with half-built entries. Specifically, a
/// directory whose meta.json was written but whose `layer/`
/// did not survive the rename, OR a directory whose `layer/`
/// exists but whose meta.json was never written, must be
/// invisible to `cmd_list` and `find_checkpoint_dir`.
#[test]
fn create_is_atomic_on_crash() {
    perms_init();
    let _g = cp_setup();

    let overlay = overlay_dir();
    let checkpoints = overlay.join("checkpoints");
    let _ = fs::remove_dir_all(&checkpoints);
    fs::create_dir_all(&checkpoints).unwrap();

    // 1) A complete checkpoint — should be visible.
    let good = checkpoints.join("001-good");
    fs::create_dir_all(good.join("layer")).unwrap();
    let meta = CheckpointMeta {
        id: "001".to_string(),
        description: "good".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        files_changed: 0,
    };
    fs::write(
        good.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();

    // 2) Crash AFTER meta.json was written but BEFORE the
    //    upper-rename completed: meta.json present, layer
    //    missing.
    let no_layer = checkpoints.join("002-no-layer");
    fs::create_dir_all(&no_layer).unwrap();
    let meta2 = CheckpointMeta {
        id: "002".to_string(),
        description: "no-layer".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        files_changed: 0,
    };
    fs::write(
        no_layer.join("meta.json"),
        serde_json::to_string_pretty(&meta2).unwrap(),
    )
    .unwrap();

    // 3) Legacy crash from the old write-meta-last code path:
    //    layer present, meta.json missing.
    let no_meta = checkpoints.join("003-no-meta");
    fs::create_dir_all(no_meta.join("layer")).unwrap();

    // 4) A hidden sentinel — the create-lock file. Must NEVER
    //    be reported.
    fs::write(checkpoints.join(".create.lock"), b"99999").unwrap();

    // cmd_list must only surface the complete checkpoint.
    let list = cmd_list(&vec![]).unwrap();
    let arr = list["checkpoints"].as_array().unwrap();
    let ids: Vec<&str> = arr.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["001"], "list must hide partial checkpoints");
    assert_eq!(list["count"], 1);

    // find_checkpoint_dir must refuse the sentinel even if
    // someone passes its literal name.
    let err = find_checkpoint_dir(&checkpoints, ".create.lock").unwrap_err();
    assert!(
        err.contains("not found"),
        "must not resolve to the create-lock sentinel: {err}"
    );

    // existing_ids must ignore the sentinel — next_checkpoint_id
    // proceeds as 003 + 1 = 004 (NOT some giant value derived
    // from the sentinel's filename).
    assert_eq!(next_checkpoint_id(&checkpoints), "004");
}

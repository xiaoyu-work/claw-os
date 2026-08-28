use super::*;
use crate::agent::run;

#[test]
fn media_default_lists_provider_registries() {
    let v = media_cmd(&[]).expect("default ok");
    assert!(v.get("outputs_dir").is_some());
    // The three registries are always present (only `noop` when
    // the active config selects `provider = "none"` for that
    // modality, which is the kernel-default state); each row
    // carries {name, configured}.
    for slot in ["tts", "stt", "imagegen"] {
        let block = v.get(slot).unwrap_or_else(|| panic!("missing {slot}"));
        let providers = block
            .get("providers")
            .and_then(|p| p.as_array())
            .unwrap_or_else(|| panic!("{slot}.providers not an array"));
        assert!(!providers.is_empty(), "{slot} has zero providers");
        let first = &providers[0];
        assert!(first.get("name").is_some());
        assert!(first.get("configured").is_some());
    }
}

#[test]
fn media_providers_default_includes_noop_in_each_registry() {
    let v = media_cmd(&["providers".into()]).expect("providers ok");
    for slot in ["tts", "stt", "imagegen"] {
        let names: Vec<String> = v
            .get(slot)
            .and_then(|s| s.get("providers"))
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            names.contains(&"noop".to_string()),
            "{slot} missing noop, got: {names:?}"
        );
    }
}

#[test]
fn media_outputs_dir_returns_path() {
    let v = media_cmd(&["outputs-dir".into()]).expect("outputs-dir ok");
    let p = v.get("path").and_then(|s| s.as_str()).expect("path field");
    assert!(p.contains("media"), "expected 'media' in path, got: {p}");
}

#[test]
fn media_list_outputs_limit_requires_int() {
    let err = media_cmd(&["list-outputs".into(), "--limit".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--limit"));
}

#[test]
fn media_list_outputs_missing_dir_returns_empty() {
    let dir = std::env::temp_dir().join(format!(
        "cos-media-list-missing-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let v = list_media_outputs(&dir, 10, None).expect("list ok");
    assert_eq!(v.get("exists").and_then(|b| b.as_bool()), Some(false));
    assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(
        v.get("files").and_then(|a| a.as_array()).map(|a| a.len()),
        Some(0)
    );
}

#[test]
fn media_list_outputs_returns_files_newest_first_within_limit() {
    let dir =
        std::env::temp_dir().join(format!("cos-media-list-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Write files with sleeps between writes so mtime ordering
    // is deterministic across Windows / Linux / macOS without
    // pulling a fresh `filetime` dep into the workspace.
    for (name, body) in [("a.png", "1"), ("b.png", "22"), ("c.wav", "333")] {
        std::fs::write(dir.join(name), body).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let v = list_media_outputs(&dir, 10, None).expect("list ok");
    assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(3));
    let files = v.get("files").and_then(|a| a.as_array()).unwrap();
    let names: Vec<&str> = files
        .iter()
        .filter_map(|f| f.get("name").and_then(|s| s.as_str()))
        .collect();
    assert_eq!(names, vec!["c.wav", "b.png", "a.png"]);
    // Filtering by ext narrows the list.
    let v2 = list_media_outputs(&dir, 10, Some("png")).expect("list png ok");
    let names2: Vec<&str> = v2
        .get("files")
        .and_then(|a| a.as_array())
        .unwrap()
        .iter()
        .filter_map(|f| f.get("name").and_then(|s| s.as_str()))
        .collect();
    assert_eq!(names2, vec!["b.png", "a.png"]);
    // Limit caps the result.
    let v3 = list_media_outputs(&dir, 1, None).expect("list lim ok");
    assert_eq!(v3.get("n").and_then(|n| n.as_u64()), Some(1));
    std::fs::remove_dir_all(&dir).ok();
}

// -----------------------------------------------------------------
// media play / playback-status
// -----------------------------------------------------------------

#[test]
fn media_play_requires_a_path() {
    let err = media_play_cmd(&[]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn media_play_rejects_extra_positional_argument() {
    let err = media_play_cmd(&["a.wav".into(), "b.wav".into()]).unwrap_err();
    assert!(err.contains("unexpected extra"), "got {err}");
}

#[test]
fn media_play_detect_only_returns_format_and_player_for_wav() {
    // --detect doesn't try to play; it just resolves the format
    // and tells you which player would be used. Safe to run on
    // CI because nothing is dispatched.
    let v = media_play_cmd(&["--detect".into(), "foo.wav".into()]).expect("ok");
    assert_eq!(v["format"], serde_json::Value::String("wav".to_string()));
    assert_eq!(v["path"].as_str().unwrap(), "foo.wav");
    // `playable` is OS-dependent; just sanity-check it's bool.
    assert!(v["playable"].is_boolean(), "got {v}");
}

#[test]
fn media_play_detect_only_returns_null_format_for_unknown_extension() {
    let v = media_play_cmd(&["--detect".into(), "foo.txt".into()]).expect("ok");
    assert!(v["format"].is_null(), "got {v}");
    assert!(v["player"].is_null(), "got {v}");
    assert_eq!(v["playable"], serde_json::Value::Bool(false));
}

#[test]
fn media_play_real_dispatch_missing_file_errs() {
    let p = format!(
        "{}\\cos-media-play-test-missing-{}.wav",
        std::env::temp_dir().display(),
        uuid::Uuid::new_v4().simple()
    );
    let err = media_play_cmd(&[p.clone()]).unwrap_err();
    assert!(err.contains("playback failed"), "got {err}");
    assert!(
        err.contains("does not exist") || err.contains("io error"),
        "got {err}"
    );
}

#[test]
fn media_playback_status_rejects_unknown_format_value() {
    let err = media_playback_status_cmd(&["--format".into(), "aac".into()]).unwrap_err();
    assert!(err.contains("aac"), "got {err}");
}

#[test]
fn media_playback_status_default_returns_all_four_formats() {
    let v = media_playback_status_cmd(&[]).expect("ok");
    let arr = v["formats"].as_array().expect("formats array");
    assert_eq!(arr.len(), 4);
    let exts: Vec<&str> = arr.iter().filter_map(|r| r["format"].as_str()).collect();
    assert!(exts.contains(&"wav"));
    assert!(exts.contains(&"mp3"));
    assert!(exts.contains(&"ogg"));
    assert!(exts.contains(&"flac"));
    assert!(v["os"].is_string(), "got {v}");
}

#[test]
fn media_playback_status_format_filter_returns_just_one_row() {
    let v = media_playback_status_cmd(&["--format".into(), "wav".into()]).expect("ok");
    let arr = v["formats"].as_array().expect("formats array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["format"].as_str().unwrap(), "wav");
}

#[test]
fn run_media_play_routes_through_dispatcher() {
    // Confirm the cos-agent dispatcher reaches media_play_cmd via `dev`.
    let err = run("dev", &["media".into(), "play".into()]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn run_media_playback_status_routes_through_dispatcher() {
    let v = run("dev", &["media".into(), "playback-status".into()]).expect("ok");
    assert!(v["formats"].is_array(), "got {v}");
}

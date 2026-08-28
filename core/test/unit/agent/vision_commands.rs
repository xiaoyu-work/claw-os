use super::*;

// ---- vision_cmd / vision_route_cmd ----

#[test]
fn vision_cmd_default_subcommand_errs_with_usage() {
    let err = vision_cmd(&[]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn vision_route_synthetic_native_when_provider_vision_and_widely_supported() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("native"));
    assert!(v.get("reason").map(|r| r.is_null()).unwrap_or(false));
}

#[test]
fn vision_route_skip_when_vision_disabled() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
        "--vision-disabled".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
    assert!(v
        .get("reason")
        .and_then(|s| s.as_str())
        .map(|r| r.contains("vision disabled"))
        .unwrap_or(false));
}

#[test]
fn vision_route_skip_when_zero_bytes() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "0".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
}

#[test]
fn vision_route_extract_text_intent_prefers_ocr_when_available() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
        "--ocr-available".into(),
        "--intent".into(),
        "extract-text".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("ocr"));
}

#[test]
fn vision_route_skip_when_oversized_and_no_ocr() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "10000000".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
        "--max-native-bytes".into(),
        "1000000".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
    assert!(v
        .get("reason")
        .and_then(|s| s.as_str())
        .map(|r| r.contains("exceeds native cap"))
        .unwrap_or(false));
}

#[test]
fn vision_route_unsupported_mime_without_ocr_skips() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/heic".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
}

#[test]
fn vision_route_requires_bytes_or_file() {
    let err = vision_route_cmd(&[]).unwrap_err();
    assert!(err.contains("--file") || err.contains("--bytes"));
}

#[test]
fn vision_route_bytes_without_mime_errs() {
    let err = vision_route_cmd(&["--bytes".into(), "1024".into()]).unwrap_err();
    assert!(err.contains("--mime"));
}

#[test]
fn vision_route_unknown_intent_errs() {
    let err = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--intent".into(),
        "bogus".into(),
    ])
    .unwrap_err();
    assert!(err.contains("bogus"));
}

#[test]
fn vision_route_file_uses_on_disk_size_and_extension_mime() {
    let dir = std::env::temp_dir().join(format!(
        "cos-agent-vision-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.png");
    std::fs::write(&path, vec![0u8; 4096]).unwrap();
    let v = vision_route_cmd(&[
        "--file".into(),
        path.display().to_string(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    let desc = v.get("descriptor").expect("descriptor");
    assert_eq!(desc.get("bytes_len").and_then(|n| n.as_u64()), Some(4096));
    assert_eq!(desc.get("mime").and_then(|m| m.as_str()), Some("Png"));
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("native"));
    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_route_file_mime_override_wins() {
    let dir = std::env::temp_dir().join(format!(
        "cos-agent-vision-mime-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.dat");
    std::fs::write(&path, vec![0u8; 100]).unwrap();
    let v = vision_route_cmd(&[
        "--file".into(),
        path.display().to_string(),
        "--mime".into(),
        "image/jpeg".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    let desc = v.get("descriptor").expect("descriptor");
    assert_eq!(desc.get("mime").and_then(|m| m.as_str()), Some("Jpeg"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_route_file_missing_path_errs() {
    let err = vision_route_cmd(&[
        "--file".into(),
        "Z:\\definitely\\does\\not\\exist.png".into(),
    ])
    .unwrap_err();
    // On unix the path also won't exist.
    assert!(err.contains("stat") || err.contains("not"));
}

// ---- vision_sniff_cmd ----

#[test]
fn vision_sniff_requires_file_or_url() {
    let err = vision_sniff_cmd(&[]).unwrap_err();
    assert!(err.contains("--file") && err.contains("--url"));
}

#[test]
fn vision_sniff_rejects_both_file_and_url() {
    let err = vision_sniff_cmd(&[
        "--file".into(),
        "x.png".into(),
        "--url".into(),
        "https://x.invalid/y".into(),
    ])
    .unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn vision_sniff_file_returns_mime_and_len() {
    // Write a tiny PNG-magic-byte stub (8-byte signature) to a temp
    // file and confirm sniff_mime classifies it.
    let dir = std::env::temp_dir().join(format!(
        "cos-vision-sniff-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.png");
    std::fs::write(
        &path,
        [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01],
    )
    .unwrap();

    let v = vision_sniff_cmd(&["--file".into(), path.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("bytes_len").and_then(|n| n.as_u64()), Some(10));
    assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Png"));
    assert_eq!(
        v.get("mime_widely_supported").and_then(|b| b.as_bool()),
        Some(true)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_sniff_file_unknown_magic_classifies_other() {
    let dir = std::env::temp_dir().join(format!(
        "cos-vision-sniff-other-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.bin");
    std::fs::write(&path, b"this is not an image").unwrap();

    let v = vision_sniff_cmd(&["--file".into(), path.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Other"));
    assert_eq!(v.get("is_other").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(
        v.get("mime_widely_supported").and_then(|b| b.as_bool()),
        Some(false)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_sniff_file_missing_path_errs() {
    let err = vision_sniff_cmd(&[
        "--file".into(),
        "Z:\\definitely\\does\\not\\exist.png".into(),
    ])
    .unwrap_err();
    assert!(err.contains("stat") || err.contains("not"));
}

#[test]
fn vision_sniff_head_bytes_caps_inspection_window() {
    let dir = std::env::temp_dir().join(format!(
        "cos-vision-sniff-head-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.png");
    // 1KB file but PNG magic in first 8 bytes.
    let mut data = vec![0u8; 1024];
    data[0..8].copy_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    std::fs::write(&path, &data).unwrap();

    let v = vision_sniff_cmd(&[
        "--file".into(),
        path.to_string_lossy().to_string(),
        "--head-bytes".into(),
        "8".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("bytes_len").and_then(|n| n.as_u64()), Some(1024));
    assert_eq!(
        v.get("head_bytes_inspected").and_then(|n| n.as_u64()),
        Some(8)
    );
    assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Png"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- vision_analyze_cmd ----

#[test]
fn vision_analyze_requires_prompt() {
    let err = vision_analyze_cmd(&["--file".into(), "x.png".into()]).unwrap_err();
    assert!(err.contains("--prompt"));
}

#[test]
fn vision_analyze_empty_prompt_errs() {
    let err = vision_analyze_cmd(&[
        "--file".into(),
        "x.png".into(),
        "--prompt".into(),
        "   ".into(),
    ])
    .unwrap_err();
    assert!(err.contains("non-empty"));
}

#[test]
fn vision_analyze_rejects_zero_image_sources() {
    let err = vision_analyze_cmd(&["--prompt".into(), "describe".into()]).unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn vision_analyze_rejects_two_image_sources() {
    let err = vision_analyze_cmd(&[
        "--file".into(),
        "x.png".into(),
        "--url".into(),
        "https://x.invalid".into(),
        "--prompt".into(),
        "describe".into(),
    ])
    .unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn vision_analyze_base64_requires_mime() {
    let err = vision_analyze_cmd(&[
        "--base64".into(),
        "AAAA".into(),
        "--prompt".into(),
        "describe".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--mime"));
}

#[test]
fn vision_analyze_file_missing_errs_clean() {
    let err = vision_analyze_cmd(&[
        "--file".into(),
        "Z:\\nope\\image.png".into(),
        "--prompt".into(),
        "describe".into(),
    ])
    .unwrap_err();
    assert!(err.contains("read"));
}

// ---- vision_cmd dispatch picks up new subcommands ----

#[test]
fn vision_cmd_routes_sniff_subcommand() {
    // Empty sniff still dispatches into vision_sniff_cmd; we just
    // assert that the error originates from that helper.
    let err = vision_cmd(&["sniff".into()]).unwrap_err();
    assert!(err.contains("--file") && err.contains("--url"));
}

#[test]
fn vision_cmd_routes_analyze_subcommand() {
    let err = vision_cmd(&["analyze".into()]).unwrap_err();
    assert!(err.contains("--prompt"));
}

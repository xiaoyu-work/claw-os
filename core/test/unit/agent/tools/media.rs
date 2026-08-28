use super::*;

#[tokio::test]
async fn tts_tool_writes_audio_and_returns_summary() {
    let reg = Arc::new(TtsRegistry::with_default_providers());
    let tool = TtsTool::new(reg);
    let r = tool
        .exec(json!({"text": "hi", "provider": "noop", "format": "wav"}))
        .await;
    assert!(!r.is_error, "got error: {}", r.content);
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["provider"], "noop");
    assert_eq!(v["format"], "wav");
    assert_eq!(v["bytes"], 44);
    let path = v["path"].as_str().unwrap();
    assert!(std::path::Path::new(path).exists());
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn tts_tool_missing_text_errors() {
    let reg = Arc::new(TtsRegistry::with_default_providers());
    let tool = TtsTool::new(reg);
    let r = tool.exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("text"));
}

#[tokio::test]
async fn tts_tool_unknown_provider_errors() {
    let reg = Arc::new(TtsRegistry::with_default_providers());
    let tool = TtsTool::new(reg);
    let r = tool.exec(json!({"text": "hi", "provider": "nope"})).await;
    assert!(r.is_error);
    assert!(r.content.contains("not registered"));
}

#[tokio::test]
async fn stt_tool_reads_file_and_transcribes() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    let dir = std::env::temp_dir().join(format!("cos-stt-test-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let audio = dir.join("clip.wav");
    std::fs::write(&audio, b"fake wav bytes").unwrap();
    let reg = Arc::new(SttRegistry::with_default_providers());
    let tool = SttTool::new(reg);
    let r = tool
        .exec(json!({"path": audio.display().to_string(), "language": "en"}))
        .await;
    assert!(!r.is_error, "got error: {}", r.content);
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["provider"], "noop");
    assert_eq!(v["language"], "en");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn stt_tool_missing_path_errors() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    let reg = Arc::new(SttRegistry::with_default_providers());
    let tool = SttTool::new(reg);
    let r = tool.exec(json!({})).await;
    assert!(r.is_error);
}

#[tokio::test]
async fn stt_tool_missing_file_errors() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
    let reg = Arc::new(SttRegistry::with_default_providers());
    let tool = SttTool::new(reg);
    // Use a path inside the user's home so the classifier doesn't
    // deny on its own; we want the test to exercise the
    // "file doesn't exist" branch.
    let dir = std::env::temp_dir().join(format!("cos-stt-missing-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("nope.wav");
    let r = tool
        .exec(json!({"path": missing.display().to_string()}))
        .await;
    assert!(r.is_error);
    assert!(r.content.contains("read audio"), "got: {}", r.content);
    std::fs::remove_dir_all(&dir).ok();
}

/// Path-safety regression: `cos_stt` must refuse paths the
/// file-safety classifier flags as Deny/Caution. Without the
/// `classify` pre-flight added in this commit the tool would
/// happily slurp `/etc/passwd` and hand the bytes off to whatever
/// STT provider the model picked — a credential exfil primitive.
#[tokio::test]
async fn stt_path_must_be_in_scope() {
    // Force permissive caps so the test specifically exercises the
    // file-safety classifier, not the (stricter) caps gate. In
    // production the two layers are independent — either refusal
    // is acceptable — but for this regression we want to pin the
    // classifier behaviour.
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let reg = Arc::new(SttRegistry::with_default_providers());
    let tool = SttTool::new(reg);
    #[cfg(unix)]
    let bad = "/etc/passwd";
    #[cfg(windows)]
    let bad = r"C:\Windows\System32\config\SAM";
    let r = tool.exec(json!({"path": bad})).await;
    assert!(r.is_error, "expected refusal for {bad}, got: {}", r.content);
    assert!(
        r.content.contains("refusing to read") && r.content.contains("file-safety"),
        "expected file-safety refusal message, got: {}",
        r.content
    );
}

#[tokio::test]
async fn imagegen_tool_writes_n_images() {
    let reg = Arc::new(ImageGenRegistry::with_default_providers());
    let tool = ImageGenTool::new(reg);
    let r = tool.exec(json!({"prompt": "a cat", "n": 2})).await;
    assert!(!r.is_error, "got error: {}", r.content);
    let v: Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(v["count"], 2);
    let paths = v["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 2);
    for p in paths {
        let path = p.as_str().unwrap();
        assert!(std::path::Path::new(path).exists());
        std::fs::remove_file(path).ok();
    }
}

#[tokio::test]
async fn imagegen_tool_missing_prompt_errors() {
    let reg = Arc::new(ImageGenRegistry::with_default_providers());
    let tool = ImageGenTool::new(reg);
    let r = tool.exec(json!({})).await;
    assert!(r.is_error);
}

#[test]
fn register_default_adds_three_tools() {
    let mut r = super::super::registry::ToolRegistry::new();
    register_default_media_tools(&mut r, std::env::temp_dir().join("cos-media-registry-test"));
    assert!(r.get("cos_tts").is_some());
    assert!(r.get("cos_stt").is_some());
    assert!(r.get("cos_imagegen").is_some());
}

#[test]
fn parse_audio_format_aliases() {
    assert_eq!(parse_audio_format("WAV"), AudioFormat::Wav);
    assert_eq!(parse_audio_format("mp3"), AudioFormat::Mp3);
    assert_eq!(parse_audio_format("pcm16"), AudioFormat::Pcm16);
    assert_eq!(parse_audio_format("zzz"), AudioFormat::Other);
}

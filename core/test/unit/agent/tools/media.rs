use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::agent::media::imagegen::{ImageGenProvider, ImageGenResponse};
use crate::agent::media::stt::{SttProvider, SttResponse};
use crate::agent::media::tts::{TtsProvider, TtsResponse};
use crate::agent::media::MediaError;
use crate::caps::{Cap, CapSet, Scope, Verb};

struct ProbeProvider {
    name: &'static str,
    configured: bool,
    calls: Arc<AtomicUsize>,
}

impl ProbeProvider {
    fn new(name: &'static str, configured: bool, calls: Arc<AtomicUsize>) -> Self {
        Self {
            name,
            configured,
            calls,
        }
    }
}

#[async_trait]
impl TtsProvider for ProbeProvider {
    fn name(&self) -> &str {
        self.name
    }

    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn synthesize(&self, _request: TtsRequest) -> Result<TtsResponse, MediaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(MediaError::Internal("probe tts invoked".to_string()))
    }
}

#[async_trait]
impl SttProvider for ProbeProvider {
    fn name(&self) -> &str {
        self.name
    }

    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn transcribe(&self, _request: SttRequest) -> Result<SttResponse, MediaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(MediaError::Internal("probe stt invoked".to_string()))
    }
}

#[async_trait]
impl ImageGenProvider for ProbeProvider {
    fn name(&self) -> &str {
        self.name
    }

    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn generate(
        &self,
        _request: ImageGenRequest,
    ) -> Result<ImageGenResponse, MediaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(MediaError::Internal("probe imagegen invoked".to_string()))
    }
}

fn media_session(caps: CapSet) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: format!("media-tool-{}", Uuid::new_v4()),
        pid: std::process::id(),
        command: vec!["cargo-test".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        client: crate::session::SessionClient::default(),
    }
}

fn scoped_media_caps(provider: &str, audio_path: &str) -> CapSet {
    CapSet::from_caps([
        Cap::new(Verb::AI_AUDIO_TTS, Scope::name(provider)),
        Cap::new(Verb::AI_AUDIO_STT, Scope::name(provider)),
        Cap::new(Verb::AI_IMAGE_GENERATE, Scope::name(provider)),
        Cap::new(Verb::FS_READ, Scope::path(audio_path)),
    ])
}

#[tokio::test]
async fn tts_tool_writes_audio_and_returns_summary() {
    let _perms = crate::test_env::PermissiveModeGuard::new();
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
    let _perms = crate::test_env::PermissiveModeGuard::new();
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
    register_default_media_tools_with_outputs_dir(
        &mut r,
        std::env::temp_dir().join("cos-media-registry-test"),
    );
    assert!(r.get("cos_tts").is_some());
    assert!(r.get("cos_stt").is_some());
    assert!(r.get("cos_imagegen").is_some());
}

#[test]
#[allow(deprecated)]
fn legacy_default_media_registration_signature_still_compiles() {
    let register: fn(&mut super::super::registry::ToolRegistry) =
        register_default_media_tools;
    let _ = register;
}

#[test]
fn parse_audio_format_aliases() {
    assert_eq!(parse_audio_format("WAV"), AudioFormat::Wav);
    assert_eq!(parse_audio_format("mp3"), AudioFormat::Mp3);
    assert_eq!(parse_audio_format("pcm16"), AudioFormat::Pcm16);
    assert_eq!(parse_audio_format("zzz"), AudioFormat::Other);
}

#[tokio::test]
async fn media_tools_project_and_enforce_exact_provider_scopes() {
    let _lock = crate::test_env::lock_env();
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "permissive");
    let dir = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("cos-media-scope-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let _caps_dir = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", &dir);
    let _log_dir = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", &dir);
    let audio = dir.join("clip.wav");
    std::fs::write(&audio, b"fake wav bytes").unwrap();
    let audio_path = audio.to_string_lossy().into_owned();

    let tts_b_calls = Arc::new(AtomicUsize::new(0));
    let stt_b_calls = Arc::new(AtomicUsize::new(0));
    let image_b_calls = Arc::new(AtomicUsize::new(0));
    let unused = Arc::new(AtomicUsize::new(0));

    let mut tts = TtsRegistry::new();
    tts.register(Arc::new(ProbeProvider::new(
        "provider-a",
        true,
        unused.clone(),
    )));
    tts.register(Arc::new(ProbeProvider::new(
        "provider-b",
        true,
        tts_b_calls.clone(),
    )));
    tts.register(Arc::new(ProbeProvider::new(
        "provider-c",
        false,
        unused.clone(),
    )));

    let mut stt = SttRegistry::new();
    stt.register(Arc::new(ProbeProvider::new(
        "provider-a",
        true,
        unused.clone(),
    )));
    stt.register(Arc::new(ProbeProvider::new(
        "provider-b",
        true,
        stt_b_calls.clone(),
    )));
    stt.register(Arc::new(ProbeProvider::new(
        "provider-c",
        false,
        unused.clone(),
    )));

    let mut imagegen = ImageGenRegistry::new();
    imagegen.register(Arc::new(ProbeProvider::new(
        "provider-a",
        true,
        unused.clone(),
    )));
    imagegen.register(Arc::new(ProbeProvider::new(
        "provider-b",
        true,
        image_b_calls.clone(),
    )));
    imagegen.register(Arc::new(ProbeProvider::new(
        "provider-c",
        false,
        unused,
    )));

    let mut registry = super::super::registry::ToolRegistry::new();
    registry.register(Arc::new(TtsTool::new(Arc::new(tts))));
    registry.register(Arc::new(SttTool::new(Arc::new(stt))));
    registry.register(Arc::new(ImageGenTool::new(Arc::new(imagegen))));

    let session = media_session(scoped_media_caps("provider-a", &audio_path));
    let context = super::super::exposure::ToolExposureContext::from_trusted_session(
        &session,
        None,
        None,
        1000,
        super::super::exposure::ExecutionHost::Direct,
        super::super::guardrails::Guardrails::permissive(),
    );
    assert_eq!(
        registry.names_for(&context),
        vec!["cos_imagegen", "cos_stt", "cos_tts"]
    );

    let unavailable_session = media_session(scoped_media_caps("provider-c", &audio_path));
    let unavailable =
        super::super::exposure::ToolExposureContext::from_trusted_session(
            &unavailable_session,
            None,
            None,
            1000,
            super::super::exposure::ExecutionHost::Direct,
            super::super::guardrails::Guardrails::permissive(),
        );
    assert!(registry.names_for(&unavailable).is_empty());

    let (tts_result, stt_result, image_result) =
        crate::proc::with_trusted_session_override(session, async {
            tokio::join!(
                registry.execute(
                    &context,
                    "cos_tts",
                    json!({"text": "hello", "provider": "provider-b"}),
                    "",
                ),
                registry.execute(
                    &context,
                    "cos_stt",
                    json!({"path": audio_path, "provider": "provider-b"}),
                    "",
                ),
                registry.execute(
                    &context,
                    "cos_imagegen",
                    json!({"prompt": "a cat", "provider": "provider-b"}),
                    "",
                ),
            )
        })
        .await;
    std::fs::remove_dir_all(&dir).ok();

    for (result, verb) in [
        (tts_result, Verb::AI_AUDIO_TTS),
        (stt_result, Verb::AI_AUDIO_STT),
        (image_result, Verb::AI_IMAGE_GENERATE),
    ] {
        assert!(result.is_error, "unauthorized provider call succeeded");
        assert!(result.content.contains(verb.as_str()), "{}", result.content);
        assert!(
            result.content.contains("name:provider-b"),
            "{}",
            result.content
        );
    }
    assert_eq!(tts_b_calls.load(Ordering::SeqCst), 0);
    assert_eq!(stt_b_calls.load(Ordering::SeqCst), 0);
    assert_eq!(image_b_calls.load(Ordering::SeqCst), 0);
}

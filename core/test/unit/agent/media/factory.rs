use super::*;

fn cfg() -> CosConfig {
    CosConfig::default()
}

// ---- TTS ---------------------------------------------------------------

#[test]
fn tts_default_provider_none_only_noop() {
    let mut c = cfg();
    c.tts.provider = "none".into();
    let reg = tts_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

#[test]
fn tts_empty_provider_only_noop() {
    let mut c = cfg();
    c.tts.provider = "".into();
    let reg = tts_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

#[test]
fn tts_unknown_provider_only_noop() {
    let mut c = cfg();
    c.tts.provider = "elavenlabs".into(); // typo
    let reg = tts_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

#[test]
fn tts_openai_alias_registers_cloud_provider() {
    let mut c = cfg();
    c.tts.provider = "openai".into();
    c.tts.model = "tts-1".into();
    let reg = tts_registry_from_cfg(&c);
    let mut names = reg.names();
    names.sort();
    assert_eq!(names, vec!["noop".to_string(), "openai".to_string()]);
    let p = reg.get("openai").unwrap();
    assert_eq!(p.name(), "openai");
}

#[test]
fn tts_xai_alias_registers_cloud_provider() {
    let mut c = cfg();
    c.tts.provider = "xai".into();
    c.tts.model = "grok-tts".into();
    let reg = tts_registry_from_cfg(&c);
    assert!(reg.get("xai").is_some());
}

#[test]
fn tts_custom_alias_uses_base_url_override() {
    let mut c = cfg();
    c.tts.provider = "custom".into();
    c.tts.model = "x-tts".into();
    c.tts.base_url = Some("https://gw.example.test/v1".into());
    let reg = tts_registry_from_cfg(&c);
    // Provider exists; correct base_url plumbing is exercised in
    // the cloud-provider's own unit tests — we only assert
    // registration here so we don't depend on internals.
    assert!(reg.get("custom").is_some());
}

#[test]
fn tts_elevenlabs_provider_registered_under_correct_name() {
    let mut c = cfg();
    c.tts.provider = "elevenlabs".into();
    c.tts.model = "eleven_multilingual_v2".into();
    let reg = tts_registry_from_cfg(&c);
    let p = reg.get("elevenlabs").expect("elevenlabs registered");
    assert_eq!(p.name(), "elevenlabs");
}

#[test]
fn tts_gemini_alias_registers() {
    let mut c = cfg();
    c.tts.provider = "gemini".into();
    let reg = tts_registry_from_cfg(&c);
    assert!(reg.get("gemini-tts").is_some());
}

#[test]
fn tts_minimax_alias_registers() {
    let mut c = cfg();
    c.tts.provider = "minimax".into();
    let reg = tts_registry_from_cfg(&c);
    assert!(reg.get("minimax").is_some());
}

#[test]
fn tts_edge_alias_registers() {
    let mut c = cfg();
    c.tts.provider = "edge".into();
    let reg = tts_registry_from_cfg(&c);
    let p = reg.get("edge-tts").expect("edge-tts registered");
    assert_eq!(p.name(), "edge-tts");
    // Edge has no API key requirement.
    assert!(p.is_configured());
}

#[test]
fn tts_edge_tts_alias_registers() {
    let mut c = cfg();
    c.tts.provider = "edge-tts".into();
    let reg = tts_registry_from_cfg(&c);
    assert!(reg.get("edge-tts").is_some());
}

#[test]
fn tts_provider_lookup_is_case_insensitive_on_alias() {
    let mut c = cfg();
    c.tts.provider = "OpenAI".into();
    let reg = tts_registry_from_cfg(&c);
    assert!(reg.get("openai").is_some());
}

#[test]
fn tts_default_voice_empty_string_treated_as_none() {
    let mut c = cfg();
    c.tts.provider = "elevenlabs".into();
    c.tts.default_voice = "".into();
    let reg = tts_registry_from_cfg(&c);
    // Just a registration check — internal handling is tested in
    // ElevenLabs provider's own suite.
    assert!(reg.get("elevenlabs").is_some());
}

// ---- STT ---------------------------------------------------------------

#[test]
fn stt_default_provider_none_only_noop() {
    let mut c = cfg();
    c.stt.provider = "none".into();
    let reg = stt_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

#[test]
fn stt_unknown_provider_only_noop() {
    let mut c = cfg();
    c.stt.provider = "watson".into();
    let reg = stt_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

#[test]
fn stt_openai_alias_registers_cloud_provider() {
    let mut c = cfg();
    c.stt.provider = "openai".into();
    c.stt.model = "whisper-1".into();
    let reg = stt_registry_from_cfg(&c);
    assert!(reg.get("openai").is_some());
}

#[test]
fn stt_groq_alias_registers_cloud_provider() {
    let mut c = cfg();
    c.stt.provider = "groq".into();
    let reg = stt_registry_from_cfg(&c);
    assert!(reg.get("groq").is_some());
}

#[test]
fn stt_mistral_alias_registers_cloud_provider() {
    let mut c = cfg();
    c.stt.provider = "mistral".into();
    let reg = stt_registry_from_cfg(&c);
    assert!(reg.get("mistral").is_some());
}

// ---- ImageGen ----------------------------------------------------------

#[test]
fn imagegen_default_provider_none_only_noop() {
    let mut c = cfg();
    c.imagegen.provider = "none".into();
    let reg = imagegen_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

#[test]
fn imagegen_openai_alias_registers() {
    let mut c = cfg();
    c.imagegen.provider = "openai".into();
    c.imagegen.model = "dall-e-3".into();
    let reg = imagegen_registry_from_cfg(&c);
    assert!(reg.get("openai").is_some());
}

#[test]
fn imagegen_xai_alias_pinned_to_xai_name() {
    let mut c = cfg();
    c.imagegen.provider = "xai".into();
    c.imagegen.model = "grok-2-image".into();
    let reg = imagegen_registry_from_cfg(&c);
    assert!(reg.get("xai").is_some());
}

#[test]
fn imagegen_fal_bare_alias_registers_under_fal() {
    let mut c = cfg();
    c.imagegen.provider = "fal".into();
    c.imagegen.model = "fal-ai/flux/dev".into();
    let reg = imagegen_registry_from_cfg(&c);
    assert!(reg.get("fal").is_some());
}

#[test]
fn imagegen_fal_prefixed_alias_preserves_user_label() {
    let mut c = cfg();
    c.imagegen.provider = "fal-flux-dev".into();
    c.imagegen.model = "fal-ai/flux/dev".into();
    let reg = imagegen_registry_from_cfg(&c);
    assert!(
        reg.get("fal-flux-dev").is_some(),
        "fal-flux-dev should be the registered alias, got {:?}",
        reg.names()
    );
}

#[test]
fn imagegen_unknown_alias_only_noop() {
    let mut c = cfg();
    c.imagegen.provider = "midjourney".into();
    let reg = imagegen_registry_from_cfg(&c);
    assert_eq!(reg.names(), vec!["noop".to_string()]);
}

// ---- duration_secs / opt_string ----------------------------------------

#[test]
fn duration_secs_zero_maps_to_zero() {
    assert_eq!(duration_secs(0), Duration::ZERO);
}

#[test]
fn duration_secs_positive_maps_to_secs() {
    assert_eq!(duration_secs(30), Duration::from_secs(30));
}

#[test]
fn opt_string_empty_is_none() {
    assert!(opt_string("").is_none());
    assert!(opt_string("   ").is_none());
}

#[test]
fn opt_string_non_empty_is_some_trimmed_kept() {
    assert_eq!(opt_string("alloy"), Some("alloy".to_string()));
    // Trailing whitespace deliberately preserved -- the provider
    // owns its own normalisation.
    assert_eq!(opt_string(" alloy "), Some(" alloy ".to_string()));
}

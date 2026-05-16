//! Media provider factory — builds [`TtsRegistry`] / [`SttRegistry`]
//! / [`ImageGenRegistry`] from a [`CosConfig`].
//!
//! Each registry always starts with the deterministic `noop` provider
//! (so call paths stay exercisable without any backend configured) and
//! then layers a single concrete cloud / local provider on top —
//! whichever one the relevant `[tts] / [stt] / [imagegen]` block in
//! the config selects via `provider = "openai"|"xai"|"elevenlabs"|...`.
//!
//! Provider name → backend mapping (TTS):
//!
//!   | `provider` value      | impl                                              |
//!   |-----------------------|---------------------------------------------------|
//!   | `none` / empty        | only `noop` registered                            |
//!   | `openai` / `xai` / `custom` | [`CloudTtsProvider`] (OpenAI `/v1/audio/speech` shape) |
//!   | `elevenlabs`          | [`ElevenLabsProvider`]                            |
//!   | `gemini` / `gemini-tts` | [`GeminiTts`]                                  |
//!   | `minimax`             | [`MiniMaxTts`]                                    |
//!   | `edge` / `edge-tts`   | [`EdgeTtsProvider`] (free, no key, WebSocket)     |
//!
//! Provider name → backend mapping (STT):
//!
//!   | `provider` value | impl                                                  |
//!   |------------------|-------------------------------------------------------|
//!   | `none` / empty   | only `noop` registered                                |
//!   | `openai` / `groq` / `xai` / `mistral` / `custom` | [`CloudSttProvider`] (OpenAI `/v1/audio/transcriptions` shape) |
//!
//! Provider name → backend mapping (Image gen):
//!
//!   | `provider` value | impl                                                  |
//!   |------------------|-------------------------------------------------------|
//!   | `none` / empty   | only `noop` registered                                |
//!   | `openai` / `custom` | [`OpenAiImageGenProvider`] (`/images/generations` shape) |
//!   | `xai`            | [`XaiImageGenProvider`] (xAI Grok image)              |
//!   | `fal` / `fal-*`  | [`FalImageGenProvider`] (FAL.ai run-anything)         |
//!
//! API-key resolution mirrors the LLM provider chain: when configured,
//! `api_key_credential` is loaded from the credential store under the
//! `agent` namespace; failing that, `api_key_env` is consulted as a
//! fallback. We deliberately reuse
//! [`crate::agent::llm::providers::openai_compat::resolve_api_key`] so
//! the precedence and empty-string handling are identical to LLM
//! providers (no second source of truth).
//!
//! Unknown `provider` values are silently ignored — `noop` stays
//! registered so call paths remain wired. The intent is: a typo in
//! `[tts] provider = "elavenlabs"` shouldn't crash the agent kernel,
//! it should leave TTS in the same shape as `provider = "none"`. The
//! `media providers` listing already shows the registered set, which
//! makes the misconfiguration visible.

use std::sync::Arc;
use std::time::Duration;

use crate::config::CosConfig;

use super::imagegen::{ImageGenProvider, ImageGenRegistry};
use super::imagegen_fal::{FalImageGenConfig, FalImageGenProvider};
use super::imagegen_openai::{OpenAiImageGenConfig, OpenAiImageGenProvider};
use super::imagegen_xai::{XaiImageGenConfig, XaiImageGenProvider};
use super::stt::{SttProvider, SttRegistry};
use super::stt_cloud::{CloudSttConfig, CloudSttProvider};
use super::tts::{TtsProvider, TtsRegistry};
use super::tts_cloud::{CloudTtsConfig, CloudTtsProvider};
use super::tts_edge::{EdgeTtsConfig, EdgeTtsProvider};
use super::tts_elevenlabs::{ElevenLabsConfig, ElevenLabsProvider};
use super::tts_gemini::{GeminiTts, GeminiTtsConfig};
use super::tts_minimax::{MiniMaxConfig, MiniMaxTts};

fn resolve_api_key(cred: Option<&str>, env: Option<&str>) -> Option<String> {
    crate::agent::llm::providers::openai_compat::resolve_api_key(cred, env)
        .ok()
        .flatten()
}

fn duration_secs(secs: u64) -> Duration {
    if secs == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(secs)
    }
}

fn opt_string(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Build a [`TtsRegistry`] from the active config. Always includes
/// `noop`; layers a single concrete provider on top when
/// `cfg.tts.provider` selects a known backend.
pub fn tts_registry_from_cfg(cfg: &CosConfig) -> TtsRegistry {
    let mut reg = TtsRegistry::with_default_providers();
    let p = &cfg.tts;
    let provider = p.provider.trim().to_lowercase();
    if provider.is_empty() || provider == "none" {
        return reg;
    }

    let api_key = resolve_api_key(p.api_key_credential.as_deref(), p.api_key_env.as_deref());
    let timeout = duration_secs(p.request_timeout);
    let extra_headers = p.extra_headers.clone();

    match provider.as_str() {
        "openai" | "xai" | "custom" => {
            let mut tcfg = CloudTtsConfig::for_alias(&provider, &p.model);
            if let Some(b) = &p.base_url {
                tcfg.base_url = b.clone();
            }
            tcfg.api_key = api_key;
            tcfg.default_voice = opt_string(&p.default_voice);
            tcfg.extra_headers = extra_headers;
            tcfg.request_timeout = timeout;
            let prov: Arc<dyn TtsProvider> = Arc::new(CloudTtsProvider::new(tcfg));
            reg.register(prov);
        }
        "elevenlabs" => {
            let mut ec = ElevenLabsConfig {
                api_key,
                model: p.model.clone(),
                default_voice_id: opt_string(&p.default_voice),
                extra_headers,
                request_timeout: timeout,
                ..ElevenLabsConfig::default()
            };
            if let Some(b) = &p.base_url {
                ec.base_url = b.clone();
            }
            reg.register(Arc::new(ElevenLabsProvider::new(ec)));
        }
        "gemini" | "gemini-tts" => {
            let mut gc = GeminiTtsConfig {
                api_key,
                model: p.model.clone(),
                default_voice: opt_string(&p.default_voice),
                extra_headers,
                request_timeout: timeout,
                ..GeminiTtsConfig::default()
            };
            if let Some(b) = &p.base_url {
                gc.base_url = b.clone();
            }
            reg.register(Arc::new(GeminiTts::new(gc)));
        }
        "minimax" => {
            let mut mc = MiniMaxConfig {
                api_key,
                model: p.model.clone(),
                default_voice_id: opt_string(&p.default_voice),
                extra_headers,
                request_timeout: timeout,
                ..MiniMaxConfig::default()
            };
            if let Some(b) = &p.base_url {
                mc.base_url = b.clone();
            }
            reg.register(Arc::new(MiniMaxTts::new(mc)));
        }
        "edge" | "edge-tts" => {
            let mut ec = EdgeTtsConfig {
                default_voice: opt_string(&p.default_voice),
                extra_headers,
                request_timeout: timeout,
                ..EdgeTtsConfig::default()
            };
            if let Some(b) = &p.base_url {
                ec.base_url = b.clone();
            }
            reg.register(Arc::new(EdgeTtsProvider::new(ec)));
        }
        _ => {
            // Unknown alias — leave only noop registered, but log
            // so the operator notices the config drift rather than
            // silently getting a noop-only registry.
            tracing::warn!(
                provider = %provider,
                "unknown tts provider; defaulting to noop only"
            );
        }
    }

    reg
}

/// Build an [`SttRegistry`] from the active config.
pub fn stt_registry_from_cfg(cfg: &CosConfig) -> SttRegistry {
    let mut reg = SttRegistry::with_default_providers();
    let p = &cfg.stt;
    let provider = p.provider.trim().to_lowercase();
    if provider.is_empty() || provider == "none" {
        return reg;
    }

    let api_key = resolve_api_key(p.api_key_credential.as_deref(), p.api_key_env.as_deref());
    let timeout = duration_secs(p.request_timeout);
    let extra_headers = p.extra_headers.clone();

    match provider.as_str() {
        "openai" | "groq" | "xai" | "mistral" | "custom" => {
            let mut sc = CloudSttConfig::for_alias(&provider, &p.model);
            if let Some(b) = &p.base_url {
                sc.base_url = b.clone();
            }
            sc.api_key = api_key;
            sc.extra_headers = extra_headers;
            sc.request_timeout = timeout;
            // CloudSttConfig has no `default_response_format` field —
            // SttRequest carries `response_hint` per call, and the
            // provider falls back to "json" when missing. The cfg's
            // `default_response_format` is consumed by callers when
            // they build the request, not at provider construction.
            let prov: Arc<dyn SttProvider> = Arc::new(CloudSttProvider::new(sc));
            reg.register(prov);
        }
        _ => {
            // Unknown alias — leave only noop registered, but log so
            // the operator notices the config drift.
            tracing::warn!(
                provider = %provider,
                "unknown stt provider; defaulting to noop only"
            );
        }
    }

    reg
}

/// Build an [`ImageGenRegistry`] from the active config.
pub fn imagegen_registry_from_cfg(cfg: &CosConfig) -> ImageGenRegistry {
    let mut reg = ImageGenRegistry::with_default_providers();
    let p = &cfg.imagegen;
    let provider = p.provider.trim().to_lowercase();
    if provider.is_empty() || provider == "none" {
        return reg;
    }

    let api_key = resolve_api_key(p.api_key_credential.as_deref(), p.api_key_env.as_deref());
    let timeout = duration_secs(p.request_timeout);
    let extra_headers = p.extra_headers.clone();

    if provider == "openai" || provider == "custom" {
        let mut oc = OpenAiImageGenConfig::for_alias(&provider, &p.model);
        if let Some(b) = &p.base_url {
            oc.base_url = b.clone();
        }
        oc.api_key = api_key;
        oc.extra_headers = extra_headers;
        oc.request_timeout = timeout;
        let prov: Arc<dyn ImageGenProvider> = Arc::new(OpenAiImageGenProvider::new(oc));
        reg.register(prov);
    } else if provider == "xai" {
        let xc = XaiImageGenConfig {
            api_key,
            model: p.model.clone(),
            extra_headers,
            request_timeout: timeout,
        };
        reg.register(Arc::new(XaiImageGenProvider::new(xc)));
    } else if provider == "fal" || provider.starts_with("fal-") {
        // For FAL, the registry alias is the same string the user
        // configured — `fal`, `fal-flux`, `fal-recraft`, etc. — so a
        // single config can pin one specific FAL model under a
        // descriptive name without ambiguity.
        let mut fc = FalImageGenConfig::new(provider.clone(), p.model.clone());
        if let Some(b) = &p.base_url {
            fc.base_url = b.clone();
        }
        fc.api_key = api_key;
        fc.extra_headers = extra_headers;
        fc.request_timeout = timeout;
        reg.register(Arc::new(FalImageGenProvider::new(fc)));
    } else {
        // Unknown alias — leave only noop registered, but log.
        tracing::warn!(
            provider = %provider,
            "unknown imagegen provider; defaulting to noop only"
        );
    }

    reg
}

#[cfg(test)]
mod tests {
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
}

//! Media-modality setup specifications and lifecycle helpers.

use serde_json::{json, Value};
use std::io::{IsTerminal, Write};

use super::{
    config_path, read_config_or_empty, read_line, split_base_url_and_api_version, store_credential,
    write_config_atomic, Modality,
};

/// Per-modality declarative wizard config.
pub(super) struct ModalitySpec {
    pub(super) name: &'static str,         // "tts" | "stt" | ...
    pub(super) config_block: &'static str, // "tts" | "stt" | ...
    headline: &'static str,                // shown at top of wizard
    next_command_hint: &'static str,
    default_provider: &'static str,
    pub(super) providers: &'static [ProviderChoice],
}

pub(super) struct ProviderChoice {
    pub(super) name: &'static str,
    pub(super) label: &'static str,       // one-line human description
    pub(super) needs_credential: bool,    // false -> no API key step
    pub(super) default_env: &'static str, // env-var fallback (empty if none)
    pub(super) sample_models: &'static [&'static str],
    pub(super) default_model: &'static str, // suggested at the prompt
}

impl ModalitySpec {
    pub(super) fn provider_choice(&self, name: &str) -> Option<&'static ProviderChoice> {
        self.providers.iter().find(|p| p.name == name)
    }
}

pub(super) fn tts_spec() -> &'static ModalitySpec {
    static SPEC: ModalitySpec = ModalitySpec {
        name: "tts",
        config_block: "tts",
        headline: "cos agent setup tts — text-to-speech wizard",
        next_command_hint: "cos agent voice 'hello world'",
        default_provider: "edge",
        providers: &[
            ProviderChoice {
                name: "edge",
                label: "Microsoft Edge voices (free, no API key required)",
                needs_credential: false,
                default_env: "",
                sample_models: &[
                    "en-US-AriaNeural",
                    "en-US-GuyNeural",
                    "zh-CN-XiaoxiaoNeural",
                ],
                default_model: "en-US-AriaNeural",
            },
            ProviderChoice {
                name: "openai",
                label: "OpenAI TTS (tts-1, tts-1-hd, gpt-4o-mini-tts)",
                needs_credential: true,
                default_env: "OPENAI_API_KEY",
                sample_models: &["tts-1", "tts-1-hd", "gpt-4o-mini-tts"],
                default_model: "tts-1",
            },
            ProviderChoice {
                name: "elevenlabs",
                label: "ElevenLabs (eleven_multilingual_v2, eleven_turbo_v2_5)",
                needs_credential: true,
                default_env: "ELEVENLABS_API_KEY",
                sample_models: &[
                    "eleven_multilingual_v2",
                    "eleven_turbo_v2_5",
                    "eleven_flash_v2_5",
                ],
                default_model: "eleven_multilingual_v2",
            },
            ProviderChoice {
                name: "gemini",
                label: "Google Gemini TTS (gemini-2.5-flash-preview-tts)",
                needs_credential: true,
                default_env: "GEMINI_API_KEY",
                sample_models: &["gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"],
                default_model: "gemini-2.5-flash-preview-tts",
            },
            ProviderChoice {
                name: "xai",
                label: "xAI (Grok TTS)",
                needs_credential: true,
                default_env: "XAI_API_KEY",
                sample_models: &["grok-2-tts"],
                default_model: "grok-2-tts",
            },
            ProviderChoice {
                name: "minimax",
                label: "MiniMax (speech-01-hd, speech-01-turbo)",
                needs_credential: true,
                default_env: "MINIMAX_API_KEY",
                sample_models: &["speech-01-hd", "speech-01-turbo"],
                default_model: "speech-01-hd",
            },
        ],
    };
    &SPEC
}

pub(super) fn stt_spec() -> &'static ModalitySpec {
    static SPEC: ModalitySpec = ModalitySpec {
        name: "stt",
        config_block: "stt",
        headline: "cos agent setup stt — speech-to-text wizard",
        next_command_hint: "cos model transcribe path/to/audio.wav",
        default_provider: "openai",
        providers: &[
            ProviderChoice {
                name: "openai",
                label: "OpenAI Whisper / gpt-4o-transcribe",
                needs_credential: true,
                default_env: "OPENAI_API_KEY",
                sample_models: &["whisper-1", "gpt-4o-mini-transcribe", "gpt-4o-transcribe"],
                default_model: "whisper-1",
            },
            ProviderChoice {
                name: "groq",
                label: "Groq (whisper-large-v3, whisper-large-v3-turbo) — fast",
                needs_credential: true,
                default_env: "GROQ_API_KEY",
                sample_models: &[
                    "whisper-large-v3",
                    "whisper-large-v3-turbo",
                    "distil-whisper-large-v3-en",
                ],
                default_model: "whisper-large-v3-turbo",
            },
            ProviderChoice {
                name: "mistral",
                label: "Mistral (voxtral-mini-2507, voxtral-small-2507)",
                needs_credential: true,
                default_env: "MISTRAL_API_KEY",
                sample_models: &["voxtral-mini-2507", "voxtral-small-2507"],
                default_model: "voxtral-mini-2507",
            },
        ],
    };
    &SPEC
}

pub(super) fn imagegen_spec() -> &'static ModalitySpec {
    static SPEC: ModalitySpec = ModalitySpec {
        name: "imagegen",
        config_block: "imagegen",
        headline: "cos agent setup imagegen — image generation wizard",
        next_command_hint: "cos agent image 'a serene mountain lake at dawn'",
        default_provider: "openai",
        providers: &[
            ProviderChoice {
                name: "openai",
                label: "OpenAI (gpt-image-1, dall-e-3, dall-e-2)",
                needs_credential: true,
                default_env: "OPENAI_API_KEY",
                sample_models: &["gpt-image-1", "dall-e-3", "dall-e-2"],
                default_model: "gpt-image-1",
            },
            ProviderChoice {
                name: "xai",
                label: "xAI (grok-2-image-1212)",
                needs_credential: true,
                default_env: "XAI_API_KEY",
                sample_models: &["grok-2-image-1212"],
                default_model: "grok-2-image-1212",
            },
            ProviderChoice {
                name: "fal",
                label: "fal.ai (flux-pro, fast-sdxl, etc.)",
                needs_credential: true,
                default_env: "FAL_KEY",
                sample_models: &[
                    "fal-ai/flux-pro/v1.1",
                    "fal-ai/flux/dev",
                    "fal-ai/fast-sdxl",
                ],
                default_model: "fal-ai/flux-pro/v1.1",
            },
        ],
    };
    &SPEC
}

pub(super) fn embed_spec() -> &'static ModalitySpec {
    static SPEC: ModalitySpec = ModalitySpec {
        name: "embed",
        config_block: "embed",
        headline: "cos agent setup embed — text-embedding wizard (semantic memory)",
        next_command_hint: "cos agent recall 'your query here'",
        default_provider: "local",
        providers: &[
            ProviderChoice {
                name: "local",
                label: "Local Qwen3-Embedding (no API key, runs on CPU)",
                needs_credential: false,
                default_env: "",
                sample_models: &["qwen3-embedding-0.6b"],
                default_model: "qwen3-embedding-0.6b",
            },
            ProviderChoice {
                name: "openai",
                label: "OpenAI embeddings (text-embedding-3-small / -large)",
                needs_credential: true,
                default_env: "OPENAI_API_KEY",
                sample_models: &[
                    "text-embedding-3-small",
                    "text-embedding-3-large",
                    "text-embedding-ada-002",
                ],
                default_model: "text-embedding-3-small",
            },
        ],
    };
    &SPEC
}

/// Shape used by `status_media` to summarise what's persisted in
/// the config for a given modality.
fn snapshot(block: &str) -> Value {
    let cfg_root_path = config_path();
    let raw = read_config_or_empty(&cfg_root_path).unwrap_or(json!({}));
    raw.get(block).cloned().unwrap_or(json!({}))
}

const LOCAL_EMBED_PROVIDER: &str = "local";
const LOCAL_EMBED_MODEL: &str = "qwen3-embedding-0.6b";

pub(super) fn is_embed_spec(spec: &ModalitySpec) -> bool {
    spec.name == "embed"
}

pub(super) fn is_local_embed_provider(provider: &str) -> bool {
    matches!(provider, "local" | "qwen3-local")
}

fn local_embed_config_from_snapshot(
    snap: &Value,
    model: Option<&str>,
) -> crate::config::EmbedConfig {
    crate::config::EmbedConfig {
        provider: LOCAL_EMBED_PROVIDER.to_string(),
        model: model
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(LOCAL_EMBED_MODEL)
            .to_string(),
        model_dir: snap
            .get("model_dir")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        ..Default::default()
    }
}

pub(super) fn local_embed_precheck(snap: &Value, model: Option<&str>) -> Result<(), String> {
    let cfg = local_embed_config_from_snapshot(snap, model);
    crate::model::tasks::qwen3_genai::precheck(&cfg)
}

pub(super) fn local_embed_unavailable_details(err: &str) -> String {
    if std::env::consts::OS == "linux" && std::env::consts::ARCH == "aarch64" {
        format!(
            "{err}. Linux arm64 builds do not bundle the local Qwen3 embedding stack because upstream ort-genai has no Linux arm64 CPU runtime yet; configure another embedding provider."
        )
    } else {
        format!(
            "{err}. Configure another embedding provider or install the local model and ort-genai runtime."
        )
    }
}

pub(super) fn default_provider_for(spec: &ModalitySpec) -> &'static str {
    if is_embed_spec(spec) && local_embed_precheck(&json!({}), None).is_err() {
        "openai"
    } else {
        spec.default_provider
    }
}

/// Run the wizard for one media modality (TTS / STT / ImageGen / Embed).
pub(super) fn wizard_media(
    spec: &'static ModalitySpec,
    verify_after: bool,
) -> Result<Value, String> {
    if !std::io::stdin().is_terminal() {
        return Err(json!({
            "error": "cos agent setup requires an interactive TTY",
            "modality": spec.name,
            "hint": format!(
                "run it in a terminal, or edit ~/.config/cos/config.json's `{}` block manually",
                spec.config_block
            ),
        })
        .to_string());
    }

    let stderr = std::io::stderr();
    let mut e = stderr.lock();
    let _ = writeln!(e, "{}", spec.headline);
    let _ = writeln!(e);

    // ---- Step 1: provider ------------------------------------------------
    let default_provider = default_provider_for(spec);
    let _ = writeln!(e, "Available providers:");
    for (i, p) in spec.providers.iter().enumerate() {
        let marker = if p.name == default_provider {
            " (default)"
        } else {
            ""
        };
        let _ = writeln!(e, "  {}. {:12} — {}{}", i + 1, p.name, p.label, marker);
    }
    let _ = writeln!(e, "  0. none — skip (clear this modality)");
    let _ = write!(
        e,
        "Pick one (0-{}, default {}): ",
        spec.providers.len(),
        spec.providers
            .iter()
            .position(|p| p.name == default_provider)
            .map(|i| (i + 1).to_string())
            .unwrap_or_else(|| "1".into())
    );
    let _ = e.flush();
    let raw = read_line()?.trim().to_string();
    let picked_idx: usize = if raw.is_empty() {
        spec.providers
            .iter()
            .position(|p| p.name == default_provider)
            .map(|i| i + 1)
            .unwrap_or(1)
    } else {
        raw.parse().map_err(|_| "expected a number".to_string())?
    };

    if picked_idx == 0 {
        // User chose to clear this modality entirely.
        return reset_media(match spec.name {
            "tts" => Modality::Tts,
            "stt" => Modality::Stt,
            "imagegen" => Modality::ImageGen,
            "embed" => Modality::Embed,
            _ => unreachable!("unknown media spec: {}", spec.name),
        });
    }
    if picked_idx > spec.providers.len() {
        return Err(format!(
            "out of range: {picked_idx} (expected 0-{})",
            spec.providers.len()
        ));
    }
    let provider = &spec.providers[picked_idx - 1];
    if is_embed_spec(spec) && is_local_embed_provider(provider.name) {
        if let Err(err) = local_embed_precheck(&json!({}), None) {
            return Err(json!({
                "error": "local embedding stack unavailable",
                "details": local_embed_unavailable_details(&err),
                "fix": "cos agent setup embed",
            })
            .to_string());
        }
    }

    // ---- Step 2: model ---------------------------------------------------
    let _ = writeln!(e);
    let _ = writeln!(e, "Common models for `{}`:", provider.name);
    for (i, m) in provider.sample_models.iter().enumerate() {
        let marker = if *m == provider.default_model {
            " (default)"
        } else {
            ""
        };
        let _ = writeln!(e, "  {}. {}{}", i + 1, m, marker);
    }
    let _ = write!(
        e,
        "Pick a number (1-{}), type a model name, or hit enter for `{}`: ",
        provider.sample_models.len(),
        provider.default_model
    );
    let _ = e.flush();
    let raw = read_line()?.trim().to_string();
    let model: String = if raw.is_empty() {
        provider.default_model.to_string()
    } else if let Ok(idx) = raw.parse::<usize>() {
        if idx < 1 || idx > provider.sample_models.len() {
            return Err(format!(
                "out of range: {idx} (expected 1-{})",
                provider.sample_models.len()
            ));
        }
        provider.sample_models[idx - 1].to_string()
    } else {
        raw
    };

    // ---- Step 3: credential ---------------------------------------------
    let mut credential_name: Option<String> = None;
    let mut credential_env: Option<String> = None;
    if provider.needs_credential {
        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "⚠  API key input is NOT hidden. Run this in a private terminal."
        );
        let _ = writeln!(
            e,
            "(Press enter without typing to use ${} from your environment instead)",
            provider.default_env
        );
        let _ = write!(
            e,
            "Paste API key for `{}` (or enter to skip): ",
            provider.name
        );
        let _ = e.flush();
        let key = read_line()?.trim().to_string();
        if key.is_empty() {
            // Env-var-only mode: no key entered, just point config at the env var.
            credential_env = Some(provider.default_env.to_string());
            let _ = writeln!(
                e,
                "→ config will read the key from ${} at runtime",
                provider.default_env
            );
        } else {
            let name = format!("{}_{}_api_key", spec.name, provider.name);
            match store_credential(&name, &key) {
                Ok(()) => {
                    let _ = writeln!(e, "✓ credential stored as `{name}` in namespace `agent`");
                    credential_name = Some(name);
                }
                Err(store_err) => {
                    let env_name = provider.default_env.to_string();
                    let _ = writeln!(e);
                    let _ = writeln!(
                        e,
                        "⚠  credential store rejected the key (likely a permission gate):"
                    );
                    let _ = writeln!(e, "   {store_err}");
                    let _ = writeln!(e);
                    let _ = writeln!(
                        e,
                        "Falling back to environment-variable mode: config will read the key"
                    );
                    let _ = writeln!(e, "from `${env_name}`. Add this to your shell rc:");
                    let _ = writeln!(e);
                    let _ = writeln!(e, "    export {env_name}='{key}'");
                    let _ = writeln!(e);
                    credential_env = Some(env_name);
                }
            }
        }
    } else {
        let _ = writeln!(
            e,
            "(provider `{}` does not need an API key; skipping credential step)",
            provider.name
        );
    }

    // ---- Step 4: persist config -----------------------------------------
    let path = config_path();
    let mut cfg = read_config_or_empty(&path)?;
    if !cfg.is_object() {
        cfg = json!({});
    }
    let root = cfg.as_object_mut().expect("ensured object above");
    let block = root
        .entry(spec.config_block.to_string())
        .or_insert_with(|| json!({}));
    if !block.is_object() {
        *block = json!({});
    }
    let block = block.as_object_mut().expect("ensured object above");
    block.insert("provider".into(), json!(provider.name));
    block.insert("model".into(), json!(model));
    if let Some(ref n) = credential_name {
        block.insert("api_key_credential".into(), json!(n));
        block.remove("api_key_env");
    } else if let Some(ref env_name) = credential_env {
        block.insert("api_key_env".into(), json!(env_name));
        block.insert("api_key_credential".into(), Value::Null);
    } else {
        block.insert("api_key_credential".into(), Value::Null);
        block.remove("api_key_env");
    }
    write_config_atomic(&path, &cfg)?;

    let _ = writeln!(e);
    let _ = writeln!(e, "✓ config written to {}", path.display());

    // ---- Step 5: light verification -------------------------------------
    // There's no per-modality live probe yet (no equivalent of
    // run_active_provider_probe for media), so we do a cheap sanity check:
    // confirm the credential is resolvable.
    let mut verified: Option<bool> = None;
    let mut probe_note: Option<String> = None;
    if verify_after && provider.needs_credential {
        let resolved = if let Some(ref n) = credential_name {
            crate::credential::try_load(n, "agent").ok().flatten()
        } else if let Some(ref env_name) = credential_env {
            std::env::var(env_name).ok()
        } else {
            None
        };
        if resolved.map(|s| !s.trim().is_empty()).unwrap_or(false) {
            let _ = writeln!(
                e,
                "✓ credential resolves (live API probe not implemented for media yet)"
            );
            verified = Some(true);
            probe_note =
                Some("credential-resolvable check only; no upstream call performed".into());
        } else {
            let _ = writeln!(e, "✗ credential is not resolvable yet — set the env var or rerun under a privileged shell");
            verified = Some(false);
            probe_note = Some("credential not resolvable from store or env".into());
        }
    }

    let _ = writeln!(e);
    let _ = writeln!(e, "Done. Try: {}", spec.next_command_hint);

    Ok(json!({
        "ok": true,
        "modality": spec.name,
        "provider": provider.name,
        "model": model,
        "api_key_credential": credential_name,
        "api_key_env": credential_env,
        "config_path": path.display().to_string(),
        "next": spec.next_command_hint,
        "verified": verified,
        "probe_note": probe_note,
    }))
}

pub(super) fn status_media(modality: Modality) -> Value {
    let spec = match modality {
        Modality::Tts => tts_spec(),
        Modality::Stt => stt_spec(),
        Modality::ImageGen => imagegen_spec(),
        Modality::Embed => embed_spec(),
        _ => return json!({"error": "not a media modality"}),
    };
    let snap = snapshot(spec.config_block);
    let raw_provider = snap
        .get("provider")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let embed_auto = matches!(modality, Modality::Embed)
        && raw_provider.as_deref().map(|p| p == "auto").unwrap_or(true);
    let auto_local_precheck = if embed_auto {
        Some(local_embed_precheck(&snap, None))
    } else {
        None
    };
    let provider = if embed_auto {
        if auto_local_precheck
            .as_ref()
            .is_some_and(|result| result.is_ok())
        {
            LOCAL_EMBED_PROVIDER.to_string()
        } else {
            "none".to_string()
        }
    } else if let Some(provider) = raw_provider {
        provider
    } else {
        "none".to_string()
    };
    let mut model = snap
        .get("model")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        if matches!(modality, Modality::Embed) && is_local_embed_provider(&provider) {
            model = LOCAL_EMBED_MODEL.to_string();
        } else if let Some(choice) = spec.providers.iter().find(|p| p.name == provider) {
            model = choice.default_model.to_string();
        }
    }
    let credential = snap
        .get("api_key_credential")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let env = snap
        .get("api_key_env")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let base_url_raw = snap
        .get("base_url")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let (endpoint, api_version) = split_base_url_and_api_version(base_url_raw.as_deref());

    // Ready iff (provider != none) AND (provider doesn't need a key OR a key is resolvable).
    let provider_choice = spec.providers.iter().find(|p| p.name == provider);
    let needs_cred = provider_choice.map(|p| p.needs_credential).unwrap_or(false);
    let key_resolvable = if !needs_cred {
        true
    } else if let Some(ref n) = credential {
        crate::credential::try_load(n, "agent")
            .ok()
            .flatten()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    } else if let Some(ref e) = env {
        std::env::var(e)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    } else {
        false
    };
    // `reason` is emitted as a structured envelope (`error` /
    // `details` / `fix`) matching the shape the cosmic-settings UI
    // expects in [`ErrorEnvelope`]. `status_llm` emits the same shape
    // via `is_ready`; emitting a plain string here used to make every
    // media settings page fail with "invalid status JSON: invalid type:
    // string, expected struct ErrorEnvelope".
    let (ready, reason): (bool, Value) = if embed_auto && provider == "none" {
        let details = auto_local_precheck
            .and_then(Result::err)
            .map(|err| local_embed_unavailable_details(&err))
            .unwrap_or_else(|| {
                "The bundled local embedding stack is not available; configure an embedding provider.".to_string()
            });
        (
            false,
            json!({
                "error": "embedding not configured",
                "details": details,
                "fix": "cos agent setup embed",
            }),
        )
    } else if matches!(modality, Modality::Embed) && is_local_embed_provider(&provider) {
        match local_embed_precheck(&snap, Some(&model)) {
            Ok(()) => (true, Value::Null),
            Err(err) => (
                false,
                json!({
                    "error": "local embedding stack unavailable",
                    "details": local_embed_unavailable_details(&err),
                    "fix": "cos agent setup embed",
                }),
            ),
        }
    } else {
        let ready = provider != "none" && !provider.is_empty() && key_resolvable;
        let reason = if provider == "none" || provider.is_empty() {
            json!({
                "error": format!("{} not configured", spec.name),
                "details": "provider is set to `none`.",
                "fix": format!("cos agent setup {}", spec.name),
            })
        } else if !key_resolvable {
            json!({
                "error": "credential missing",
                "details": format!(
                    "provider `{provider}` needs a credential; none resolvable from \
                     store or env."
                ),
                "fix": format!("cos agent setup {}", spec.name),
            })
        } else {
            Value::Null
        };
        (ready, reason)
    };

    json!({
        "modality": spec.name,
        "config_block": spec.config_block,
        "ready": ready,
        "provider": provider,
        "model": model,
        "api_key_credential": credential,
        "api_key_env": env,
        "base_url": base_url_raw,
        "endpoint": endpoint,
        "api_version": api_version,
        "reason": reason,
    })
}

/// Reset a single media modality's config block to `provider=none`.
pub(super) fn reset_media(modality: Modality) -> Result<Value, String> {
    let block_name = match modality {
        Modality::Tts => "tts",
        Modality::Stt => "stt",
        Modality::ImageGen => "imagegen",
        Modality::Embed => "embed",
        _ => return Err(format!("not a media modality: {}", modality.name())),
    };
    let path = config_path();
    let mut cfg = read_config_or_empty(&path)?;
    if !cfg.is_object() {
        cfg = json!({});
    }
    let root = cfg.as_object_mut().expect("ensured object above");
    let block = root
        .entry(block_name.to_string())
        .or_insert_with(|| json!({}));
    if !block.is_object() {
        *block = json!({});
    }
    let block = block.as_object_mut().expect("ensured object above");
    block.insert("provider".into(), json!("none"));
    block.insert("api_key_credential".into(), Value::Null);
    block.remove("api_key_env");
    write_config_atomic(&path, &cfg)?;
    Ok(json!({
        "ok": true,
        "modality": block_name,
        "message": format!("`{block_name}` config reset to provider=none"),
        "config_path": path.display().to_string(),
    }))
}

/// Light verification for media modalities (credential-resolvable only —
/// no upstream call yet).
pub(super) fn verify_media(modality: Modality) -> Value {
    let snap = status_media(modality);
    let ready = snap.get("ready").and_then(|b| b.as_bool()).unwrap_or(false);
    json!({
        "modality": modality.name(),
        "ok": ready,
        "attempted": true,
        "kind": "credential-resolvable check (no upstream API call)",
        "reason": snap.get("reason").cloned().unwrap_or(Value::Null),
        "provider": snap.get("provider").cloned().unwrap_or(Value::Null),
        "model": snap.get("model").cloned().unwrap_or(Value::Null),
        "hint": if ready {
            Value::Null
        } else {
            json!(format!("run `cos agent setup {}` to reconfigure", modality.name()))
        },
    })
}

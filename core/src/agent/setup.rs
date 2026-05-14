//! `cos agent setup` — interactive first-run wizard.
//!
//! Replaces the previous `cos agent onboarding` family. Prompts the
//! user for an LLM provider, a default model, and an API key, then
//! writes them to the credential store and `/etc/cos/config.json` (or
//! `$COS_CONFIG_PATH`) so `cos agent chat` / `ask` work immediately.
//!
//! Subcommands:
//!   * (no args)  Run the interactive wizard. Requires a TTY.
//!   * `--status` Read-only: is the agent configured to talk to a real
//!                provider and is its credential resolvable?
//!   * `--reset`  Revert the agent block to the built-in mock defaults.
//!
//! The source of truth for "is the agent ready?" is the live config +
//! credential store (`is_ready`), never a separate state file.

use serde_json::{json, Value};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::agent::llm;

pub fn run(args: &[String]) -> Result<Value, String> {
    // Parse: optional --no-verify / --verify-only / --status / --reset / --help
    // plus a leading positional modality: llm | tts | stt | imagegen |
    // embed | all. The bare wizard with no positional defaults to `llm`
    // since that's the modality the agent itself can't function without.
    let mut verify_after = true;
    let mut explicit_verify = false;
    let mut sub: Option<&str> = None;
    let mut modality: Option<Modality> = None;

    for a in args {
        match a.as_str() {
            "--no-verify" => verify_after = false,
            "--verify" => verify_after = true,
            "--verify-only" => explicit_verify = true,
            "--status" | "status" if sub.is_none() => sub = Some("status"),
            "--reset" | "reset" if sub.is_none() => sub = Some("reset"),
            "-h" | "--help" if sub.is_none() => sub = Some("help"),
            other => {
                if let Some(m) = Modality::parse(other) {
                    if modality.is_some() {
                        return Err(format!(
                            "specify a modality only once (got both `{}` and `{other}`)",
                            modality.unwrap().name()
                        ));
                    }
                    modality = Some(m);
                } else if other.starts_with('-') {
                    return Err(format!(
                        "unknown setup flag: {other}. try: --no-verify | --verify-only | --status | --reset"
                    ));
                } else if sub.is_none() {
                    return Err(format!(
                        "unknown setup subcommand: {other}. try: (no args for llm wizard) | llm | tts | stt | imagegen | embed | all | --status | --reset | --verify-only"
                    ));
                }
            }
        }
    }

    let modality = modality.unwrap_or(Modality::Llm);

    if explicit_verify {
        return verify_cmd(modality);
    }
    match sub {
        Some("status") => status_cmd(modality),
        Some("reset") => reset_cmd(modality),
        Some("help") => Ok(help_doc()),
        _ => match modality {
            Modality::Llm => wizard_llm(verify_after),
            Modality::Tts => wizard_media(media::tts_spec(), verify_after),
            Modality::Stt => wizard_media(media::stt_spec(), verify_after),
            Modality::ImageGen => wizard_media(media::imagegen_spec(), verify_after),
            Modality::Embed => wizard_media(media::embed_spec(), verify_after),
            Modality::All => wizard_all(verify_after),
        },
    }
}

/// Which model class this invocation of `cos agent setup` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    /// Conversational LLM under `[agent]`. The default — the agent
    /// can't really run without this one.
    Llm,
    /// Text-to-speech under `[tts]`.
    Tts,
    /// Speech-to-text under `[stt]`.
    Stt,
    /// Image generation under `[imagegen]`.
    ImageGen,
    /// Text-embedding (semantic memory) under `[embed]`.
    Embed,
    /// Run every modality wizard in sequence, asking before each.
    All,
}

impl Modality {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "llm" | "agent" => Some(Self::Llm),
            "tts" | "speech" => Some(Self::Tts),
            "stt" | "transcribe" | "asr" => Some(Self::Stt),
            "imagegen" | "image" | "image-gen" => Some(Self::ImageGen),
            "embed" | "embedding" | "embeddings" => Some(Self::Embed),
            "all" => Some(Self::All),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Tts => "tts",
            Self::Stt => "stt",
            Self::ImageGen => "imagegen",
            Self::Embed => "embed",
            Self::All => "all",
        }
    }
}

fn help_doc() -> Value {
    json!({
        "command": "cos agent setup [MODALITY]",
        "summary": "Per-modality config wizard: pick a provider, a model, store an API key, and verify it works.",
        "modalities": {
            "(default)": "Same as `llm` — first-time setup almost always means the conversational LLM.",
            "llm":       "Conversational LLM (under [agent]). Required for `cos agent ask`/`chat`.",
            "tts":       "Text-to-speech (under [tts]). Used by voice output / `cos agent voice`.",
            "stt":       "Speech-to-text (under [stt]). Used by voice input / `cos agent transcribe`.",
            "imagegen":  "Image generation (under [imagegen]). Used by `cos agent image`.",
            "embed":     "Text embeddings (under [embed]). Used by semantic memory / `cos agent recall`.",
            "all":       "Walk every modality in order, prompting before each.",
        },
        "flags": {
            "--no-verify":   "Skip the live provider probe at the end of the wizard.",
            "--verify-only": "Skip the wizard; just probe the currently configured provider for the given modality (or `all`).",
            "--status":      "Show whether the picked modality is configured. With `all`, shows every modality.",
            "--reset":       "Revert the picked modality's config block to its built-in defaults (`mock`/`none`).",
        },
        "examples": [
            "cos agent setup                 # wizard for LLM",
            "cos agent setup tts             # wizard for text-to-speech",
            "cos agent setup all             # walk every modality, asking before each",
            "cos agent setup all --status    # report readiness across all modalities",
            "cos agent setup imagegen --verify-only",
        ],
    })
}

/// Probe the currently configured provider without re-running the
/// wizard. Useful after editing `/etc/cos/config.json` by hand or
/// rotating an API key.
fn verify_cmd(modality: Modality) -> Result<Value, String> {
    match modality {
        Modality::Llm => verify_llm(),
        Modality::All => {
            let mut report = serde_json::Map::new();
            report.insert("llm".into(), verify_llm().unwrap_or_else(|e| json!({"error": e})));
            for m in [Modality::Tts, Modality::Stt, Modality::ImageGen, Modality::Embed] {
                report.insert(m.name().into(), verify_media(m));
            }
            Ok(json!({"verified": report}))
        }
        other => Ok(verify_media(other)),
    }
}

fn verify_llm() -> Result<Value, String> {
    let cfg = &crate::config::get().agent;
    if let Err(reason) = is_ready(cfg) {
        let reason_val: Value =
            serde_json::from_str(&reason).unwrap_or_else(|_| json!(reason));
        return Ok(json!({
            "modality": "llm",
            "ok": false,
            "attempted": false,
            "reason": reason_val,
            "hint": "run `cos agent setup` to configure a provider first",
        }));
    }
    if !provider_needs_credential(&cfg.provider) {
        return Ok(json!({
            "modality": "llm",
            "ok": true,
            "attempted": false,
            "reason": format!("provider `{}` does not need credentials; skipping probe", cfg.provider),
            "provider": cfg.provider,
            "model": cfg.model,
        }));
    }
    let mut e = std::io::stderr();
    let _ = writeln!(e, "probing llm: {} ({}) — up to 30s...", cfg.provider, cfg.model);
    let verdict = super::run_active_provider_probe(&cfg.provider, cfg, 30);
    let ok = verdict.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        let _ = writeln!(e, "✓ probe succeeded");
    } else {
        let _ = writeln!(e, "✗ probe failed");
        if let Some(msg) = verdict.get("error_message").and_then(|v| v.as_str()) {
            let _ = writeln!(e, "  {msg}");
        }
        let _ = writeln!(e, "  hint: re-run `cos agent setup llm` or fix the credential, then `cos agent setup llm --verify-only`");
    }
    Ok(json!({
        "modality": "llm",
        "ok": ok,
        "attempted": true,
        "provider": cfg.provider,
        "model": cfg.model,
        "probe": verdict,
    }))
}

// ---------------------------------------------------------------------------
// Readiness gate (used by ask/chat/live/stream)
// ---------------------------------------------------------------------------

/// Whether `cos agent {ask,chat,live,stream}` can usefully run.
///
/// Returns `Err(json)` with a structured payload pointing at
/// `cos agent setup` when the agent is still on the mock provider or
/// the configured provider has no resolvable credential.
pub fn is_ready(cfg: &crate::config::AgentConfig) -> Result<(), String> {
    if cfg.provider == "mock" {
        return Err(json!({
            "error": "agent not configured",
            "fix": "cos agent setup",
            "details": "the default provider is `mock` (returns canned answers). Run `cos agent setup` to pick a real LLM provider.",
        })
        .to_string());
    }
    if !provider_needs_credential(&cfg.provider) {
        return Ok(());
    }
    if credential_present(cfg) {
        return Ok(());
    }
    Err(json!({
        "error": "agent provider configured but no credential found",
        "provider": cfg.provider,
        "fix": "cos agent setup",
        "details": format!(
            "no credential resolvable for provider `{}` (checked credential store namespace `agent` and env vars).",
            cfg.provider
        ),
    })
    .to_string())
}

pub fn provider_needs_credential(name: &str) -> bool {
    !matches!(name, "mock" | "llama_local")
}

fn credential_present(cfg: &crate::config::AgentConfig) -> bool {
    resolved_key_source(cfg).is_some()
}

/// Returns a structured description of which credential/env actually
/// resolved a non-empty API key for `cfg`, or `None` if no source
/// resolved. Used by both the readiness gate and `cos agent status`
/// so they agree on what "key present" means.
pub fn resolved_key_source(cfg: &crate::config::AgentConfig) -> Option<KeySource> {
    if let Some(name) = cfg.api_key_credential.as_deref() {
        if let Ok(Some(v)) = crate::credential::try_load(name, "agent") {
            if !v.trim().is_empty() {
                return Some(KeySource::credential(name));
            }
        }
    }
    if let Some(env_name) = cfg.api_key_env.as_deref() {
        if let Ok(v) = std::env::var(env_name) {
            if !v.trim().is_empty() {
                return Some(KeySource::env(env_name));
            }
        }
    }
    for name in &cfg.api_key_credentials {
        if let Ok(Some(v)) = crate::credential::try_load(name, "agent") {
            if !v.trim().is_empty() {
                return Some(KeySource::credential(name));
            }
        }
    }
    for env_name in &cfg.api_key_envs {
        if let Ok(v) = std::env::var(env_name) {
            if !v.trim().is_empty() {
                return Some(KeySource::env(env_name));
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct KeySource {
    pub kind: &'static str,
    pub name: String,
}
impl KeySource {
    fn credential(name: &str) -> Self {
        Self { kind: "credential", name: name.to_string() }
    }
    fn env(name: &str) -> Self {
        Self { kind: "env", name: name.to_string() }
    }
    pub fn to_json(&self) -> Value {
        json!({ "kind": self.kind, "name": self.name })
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn status_cmd(modality: Modality) -> Result<Value, String> {
    match modality {
        Modality::Llm => Ok(status_llm()),
        Modality::All => {
            let mut map = serde_json::Map::new();
            map.insert("llm".into(), status_llm());
            for m in [Modality::Tts, Modality::Stt, Modality::ImageGen, Modality::Embed] {
                map.insert(m.name().into(), status_media(m));
            }
            map.insert("config_path".into(), json!(config_path().display().to_string()));
            Ok(json!({"modalities": map}))
        }
        other => Ok(status_media(other)),
    }
}

fn status_llm() -> Value {
    let cfg = &crate::config::get().agent;
    let ready = is_ready(cfg);
    let reason = match ready.as_ref() {
        Ok(_) => Value::Null,
        Err(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!(s)),
    };
    json!({
        "modality": "llm",
        "ready": ready.is_ok(),
        "provider": cfg.provider,
        "model": cfg.model,
        "api_key_credential": cfg.api_key_credential,
        "api_key_env": cfg.api_key_env,
        "config_path": config_path().display().to_string(),
        "reason": reason,
    })
}

fn reset_cmd(modality: Modality) -> Result<Value, String> {
    match modality {
        Modality::Llm => reset_llm(),
        Modality::All => {
            reset_llm()?;
            for m in [Modality::Tts, Modality::Stt, Modality::ImageGen, Modality::Embed] {
                let _ = reset_media(m);
            }
            Ok(json!({
                "ok": true,
                "message": "all modalities reset",
                "config_path": config_path().display().to_string(),
            }))
        }
        other => reset_media(other),
    }
}

fn reset_llm() -> Result<Value, String> {
    let path = config_path();
    let mut cfg = read_config_or_empty(&path)?;
    if !cfg.is_object() {
        cfg = json!({});
    }
    let root = cfg.as_object_mut().expect("ensured object above");
    let agent = root
        .entry("agent".to_string())
        .or_insert_with(|| json!({}));
    if !agent.is_object() {
        *agent = json!({});
    }
    let agent = agent.as_object_mut().expect("ensured object above");
    agent.insert("provider".into(), json!("mock"));
    agent.insert("model".into(), json!("mock-model"));
    agent.insert("api_key_credential".into(), Value::Null);
    write_config_atomic(&path, &cfg)?;
    Ok(json!({
        "ok": true,
        "message": "agent config reset to mock provider",
        "config_path": path.display().to_string(),
    }))
}

fn wizard_llm(verify_after: bool) -> Result<Value, String> {
    if !std::io::stdin().is_terminal() {
        return Err(json!({
            "error": "cos agent setup requires an interactive TTY",
            "hint": "run it in a terminal, or write /etc/cos/config.json and the `agent` credential manually",
        })
        .to_string());
    }

    let stderr = std::io::stderr();
    let mut e = stderr.lock();
    let _ = writeln!(e, "cos agent setup — interactive first-run wizard");
    let _ = writeln!(e);

    // ---- Step 1: provider ------------------------------------------------
    let providers = llm::available_providers();
    if providers.is_empty() {
        return Err("no LLM providers linked into this build".into());
    }
    let _ = writeln!(e, "Available providers:");
    for (i, p) in providers.iter().enumerate() {
        let _ = writeln!(e, "  {}. {}", i + 1, p);
    }
    let _ = write!(e, "Pick one (1-{}): ", providers.len());
    let _ = e.flush();
    let picked: usize = read_line()?
        .trim()
        .parse()
        .map_err(|_| "expected a number".to_string())?;
    if picked < 1 || picked > providers.len() {
        return Err(format!("out of range: {picked}"));
    }
    let provider = providers[picked - 1].to_string();

    // ---- Step 2: model ---------------------------------------------------
    // The provider trait's `supported_models()` mostly echoes the
    // configured value — useless for validation. We use the static
    // `llm::metadata` catalogue instead, which has real names, context
    // windows, and pricing for the major providers. When the catalogue
    // is non-empty we let the user pick by number; when it's empty
    // (e.g. `openai_compat`, `ollama`) we fall back to free-form.
    let known = llm::metadata::list_for_provider(&provider);
    let _ = writeln!(e);
    let model = if !known.is_empty() {
        let _ = writeln!(e, "Known models for `{provider}`:");
        for (i, m) in known.iter().enumerate() {
            let _ = writeln!(
                e,
                "  {:>2}. {:30} ({} ctx)",
                i + 1,
                m.name,
                fmt_ctx_window(m.context_window),
            );
        }
        let _ = write!(
            e,
            "Pick a number (1-{}) or type a model name: ",
            known.len()
        );
        let _ = e.flush();
        let raw = read_line()?.trim().to_string();
        if raw.is_empty() {
            return Err("model name cannot be empty".into());
        }
        if let Ok(idx) = raw.parse::<usize>() {
            if idx < 1 || idx > known.len() {
                return Err(format!(
                    "out of range: {idx} (expected 1-{})",
                    known.len()
                ));
            }
            known[idx - 1].name.to_string()
        } else {
            // Free-form. If it's not in the known table for ANY provider,
            // warn and confirm. If it's in the table but for a different
            // provider, surface that mismatch loudly — picking `gpt-4o`
            // under provider `anthropic` is almost always a mistake.
            match llm::metadata::lookup(&raw) {
                Some(m) if m.provider.eq_ignore_ascii_case(&provider) => raw,
                Some(m) => {
                    let _ = writeln!(e);
                    let _ = writeln!(
                        e,
                        "⚠  `{}` is registered as a `{}` model, not `{provider}`.",
                        raw, m.provider
                    );
                    let _ = write!(e, "Use it anyway? (y/N): ");
                    let _ = e.flush();
                    let yn = read_line()?.trim().to_ascii_lowercase();
                    if !matches!(yn.as_str(), "y" | "yes") {
                        return Err(format!(
                            "model `{raw}` belongs to provider `{}`, refusing under `{provider}`",
                            m.provider
                        ));
                    }
                    raw
                }
                None => {
                    let _ = writeln!(e);
                    let _ = writeln!(
                        e,
                        "⚠  `{raw}` is not in the bundled known-models catalogue for `{provider}`."
                    );
                    let _ = writeln!(
                        e,
                        "   (That just means we have no pricing / context-window metadata —"
                    );
                    let _ = writeln!(e, "   the provider may still recognise it.)");
                    let _ = write!(e, "Use it anyway? (y/N): ");
                    let _ = e.flush();
                    let yn = read_line()?.trim().to_ascii_lowercase();
                    if !matches!(yn.as_str(), "y" | "yes") {
                        return Err("aborted: unknown model name".into());
                    }
                    raw
                }
            }
        }
    } else {
        let _ = writeln!(
            e,
            "No bundled model catalogue for `{provider}` — enter the model identifier"
        );
        let _ = writeln!(
            e,
            "exactly as the upstream API expects it (e.g. `llama3.2:3b` for Ollama)."
        );
        let _ = write!(e, "Model name: ");
        let _ = e.flush();
        let raw = read_line()?.trim().to_string();
        if raw.is_empty() {
            return Err("model name cannot be empty".into());
        }
        raw
    };

    // ---- Step 3: credential ---------------------------------------------
    let mut credential_name: Option<String> = None;
    let mut credential_env: Option<String> = None;
    if provider_needs_credential(&provider) {
        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "⚠  API key input is NOT hidden. Run this in a private terminal."
        );
        let _ = write!(e, "Paste API key for `{provider}`: ");
        let _ = e.flush();
        let key = read_line()?.trim().to_string();
        if key.is_empty() {
            return Err("API key cannot be empty".into());
        }
        let name = format!("{provider}_api_key");
        match store_credential(&name, &key) {
            Ok(()) => {
                let _ = writeln!(
                    e,
                    "✓ credential stored as `{name}` in namespace `agent`"
                );
                credential_name = Some(name);
            }
            Err(store_err) => {
                let env_name = default_env_name(&provider);
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
                let _ = writeln!(
                    e,
                    "from `${env_name}`. Add this to your shell rc (or systemd EnvironmentFile):"
                );
                let _ = writeln!(e);
                let _ = writeln!(e, "    export {env_name}='{key}'");
                let _ = writeln!(e);
                let _ = writeln!(
                    e,
                    "To store the key in the encrypted credential store instead, rerun under"
                );
                let _ = writeln!(
                    e,
                    "a privileged session (e.g. `sudo COS_CONFIG_PATH=$COS_CONFIG_PATH cos agent setup`)."
                );
                credential_env = Some(env_name);
            }
        }
    } else {
        let _ = writeln!(
            e,
            "(provider `{provider}` does not need an API key; skipping credential step)"
        );
    }

    // ---- Step 4: persist config ----------------------------------------
    let path = config_path();
    let mut cfg = read_config_or_empty(&path)?;
    if !cfg.is_object() {
        cfg = json!({});
    }
    let root = cfg.as_object_mut().expect("ensured object above");
    let agent = root
        .entry("agent".to_string())
        .or_insert_with(|| json!({}));
    if !agent.is_object() {
        *agent = json!({});
    }
    let agent = agent.as_object_mut().expect("ensured object above");
    agent.insert("provider".into(), json!(provider));
    agent.insert("model".into(), json!(model));
    if let Some(ref n) = credential_name {
        agent.insert("api_key_credential".into(), json!(n));
        agent.remove("api_key_env");
    } else if let Some(ref env_name) = credential_env {
        agent.insert("api_key_env".into(), json!(env_name));
        agent.insert("api_key_credential".into(), Value::Null);
    }
    write_config_atomic(&path, &cfg)?;

    let _ = writeln!(e);
    let _ = writeln!(e, "✓ config written to {}", path.display());

    // ---- Step 5: optionally verify the key actually works ---------------
    //
    // We re-read the config so the probe sees exactly what was persisted
    // (env / credential resolution happens through the same code path
    // every other agent command uses), and we explicitly skip the probe
    // for providers that don't talk to an upstream over the network.
    let mut probe_value: Value = Value::Null;
    let mut probe_ok: Option<bool> = None;
    let needs_probe = verify_after && provider_needs_credential(&provider);
    if !verify_after {
        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "(skipping live probe; re-run with `cos agent setup --verify-only` to confirm later)"
        );
    } else if !needs_probe {
        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "(provider `{provider}` does not need a credential probe)"
        );
    } else {
        let _ = writeln!(e);
        let _ = writeln!(e, "verifying connectivity to {provider} ({model}) — up to 30s...");
        // The global config is cached at first access (OnceLock) so it
        // does not reflect what we just persisted. Construct a probe
        // config in-memory by cloning the cached one and overriding the
        // fields the wizard just wrote — this matches exactly what
        // `cos agent {ask,chat}` will see on next process start.
        let mut probe_cfg = crate::config::get().agent.clone();
        probe_cfg.provider = provider.clone();
        probe_cfg.model = model.clone();
        probe_cfg.api_key_credential = credential_name.clone();
        probe_cfg.api_key_env = credential_env.clone();
        let verdict = super::run_active_provider_probe(&provider, &probe_cfg, 30);
        let ok = verdict.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if ok {
            let _ = writeln!(e, "✓ provider responded successfully");
        } else {
            let _ = writeln!(e, "✗ probe failed");
            if let Some(msg) = verdict.get("error_message").and_then(|v| v.as_str()) {
                let _ = writeln!(e, "  {msg}");
            }
            let _ = writeln!(
                e,
                "  hint: fix the credential, then run `cos agent setup --verify-only` to retest (no re-wizard)."
            );
        }
        probe_ok = Some(ok);
        probe_value = verdict;
    }

    let _ = writeln!(e);
    let _ = writeln!(e, "Done. Try: cos agent chat");

    Ok(json!({
        "ok": true,
        "provider": provider,
        "model": model,
        "api_key_credential": credential_name,
        "api_key_env": credential_env,
        "config_path": path.display().to_string(),
        "next": "cos agent chat",
        "verified": probe_ok,
        "probe": probe_value,
    }))
}

// ---------------------------------------------------------------------------
// Media modality wizards (TTS / STT / ImageGen / Embed)
//
// Same shape as wizard_llm — pick provider, pick model, store
// credential, persist, optionally verify — but driven by a static
// per-modality spec instead of `llm::metadata`. Media providers don't
// have a bundled catalogue, so the model step uses the spec's
// `sample_models` as hints and accepts any free-form name.
// ---------------------------------------------------------------------------

mod media {
    use serde_json::{json, Value};

    /// Per-modality declarative wizard config.
    pub(super) struct ModalitySpec {
        pub name: &'static str,            // "tts" | "stt" | ...
        pub config_block: &'static str,    // "tts" | "stt" | ...
        pub headline: &'static str,        // shown at top of wizard
        pub next_command_hint: &'static str,
        pub default_provider: &'static str,
        pub providers: &'static [ProviderChoice],
    }

    pub(super) struct ProviderChoice {
        pub name: &'static str,
        pub label: &'static str,           // one-line human description
        pub needs_credential: bool,        // false → no API key step
        pub default_env: &'static str,     // env-var fallback (empty if none)
        pub sample_models: &'static [&'static str],
        pub default_model: &'static str,   // suggested at the prompt
    }

    impl ModalitySpec {
        #[allow(dead_code)]
        pub fn provider_choice(&self, name: &str) -> Option<&'static ProviderChoice> {
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
                    sample_models: &["en-US-AriaNeural", "en-US-GuyNeural", "zh-CN-XiaoxiaoNeural"],
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
                    sample_models: &["eleven_multilingual_v2", "eleven_turbo_v2_5", "eleven_flash_v2_5"],
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
            next_command_hint: "cos agent transcribe path/to/audio.wav",
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
                    sample_models: &["whisper-large-v3", "whisper-large-v3-turbo", "distil-whisper-large-v3-en"],
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
                ProviderChoice {
                    name: "xai",
                    label: "xAI",
                    needs_credential: true,
                    default_env: "XAI_API_KEY",
                    sample_models: &["grok-2-stt"],
                    default_model: "grok-2-stt",
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
                    sample_models: &["fal-ai/flux-pro/v1.1", "fal-ai/flux/dev", "fal-ai/fast-sdxl"],
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
                    sample_models: &["text-embedding-3-small", "text-embedding-3-large", "text-embedding-ada-002"],
                    default_model: "text-embedding-3-small",
                },
            ],
        };
        &SPEC
    }

    /// Shape used by `status_media` to summarise what's persisted in
    /// the config for a given modality.
    pub(super) fn snapshot(block: &str) -> Value {
        let cfg_root_path = super::config_path();
        let raw = super::read_config_or_empty(&cfg_root_path).unwrap_or(json!({}));
        raw.get(block).cloned().unwrap_or(json!({}))
    }
}

/// Run the wizard for one media modality (TTS / STT / ImageGen / Embed).
fn wizard_media(spec: &'static media::ModalitySpec, verify_after: bool) -> Result<Value, String> {
    if !std::io::stdin().is_terminal() {
        return Err(json!({
            "error": "cos agent setup requires an interactive TTY",
            "modality": spec.name,
            "hint": format!(
                "run it in a terminal, or edit /etc/cos/config.json's `{}` block manually",
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
    let _ = writeln!(e, "Available providers:");
    for (i, p) in spec.providers.iter().enumerate() {
        let marker = if p.name == spec.default_provider { " (default)" } else { "" };
        let _ = writeln!(e, "  {}. {:12} — {}{}", i + 1, p.name, p.label, marker);
    }
    let _ = writeln!(e, "  0. none — skip (clear this modality)");
    let _ = write!(
        e,
        "Pick one (0-{}, default {}): ",
        spec.providers.len(),
        spec.providers
            .iter()
            .position(|p| p.name == spec.default_provider)
            .map(|i| (i + 1).to_string())
            .unwrap_or_else(|| "1".into())
    );
    let _ = e.flush();
    let raw = read_line()?.trim().to_string();
    let picked_idx: usize = if raw.is_empty() {
        spec.providers
            .iter()
            .position(|p| p.name == spec.default_provider)
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

    // ---- Step 2: model ---------------------------------------------------
    let _ = writeln!(e);
    let _ = writeln!(e, "Common models for `{}`:", provider.name);
    for (i, m) in provider.sample_models.iter().enumerate() {
        let marker = if *m == provider.default_model { " (default)" } else { "" };
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
        let _ = write!(e, "Paste API key for `{}` (or enter to skip): ", provider.name);
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
            let _ = writeln!(e, "✓ credential resolves (live API probe not implemented for media yet)");
            verified = Some(true);
            probe_note = Some("credential-resolvable check only; no upstream call performed".into());
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

/// Walk every modality, asking "Configure X? [y/N]" before each.
fn wizard_all(verify_after: bool) -> Result<Value, String> {
    if !std::io::stdin().is_terminal() {
        return Err(json!({
            "error": "cos agent setup all requires an interactive TTY",
            "hint": "set up each modality non-interactively by editing /etc/cos/config.json",
        })
        .to_string());
    }

    let stderr = std::io::stderr();
    let mut e = stderr.lock();
    let _ = writeln!(e, "cos agent setup all — walking every modality");
    let _ = writeln!(e);

    let modalities = [
        ("llm",      "Conversational LLM (required for `ask`/`chat`)"),
        ("tts",      "Text-to-speech"),
        ("stt",      "Speech-to-text"),
        ("imagegen", "Image generation"),
        ("embed",    "Text embeddings (semantic memory)"),
    ];

    let mut results = serde_json::Map::new();
    for (name, label) in modalities {
        let modality = Modality::parse(name).expect("static modality name");
        let current = match modality {
            Modality::Llm => status_llm(),
            Modality::Tts | Modality::Stt | Modality::ImageGen | Modality::Embed => status_media(modality),
            _ => continue,
        };
        let provider = current.get("provider").and_then(|s| s.as_str()).unwrap_or("");
        let ready = current.get("ready").and_then(|b| b.as_bool()).unwrap_or(false);
        let badge = if ready { "✓ ready" } else if provider.is_empty() || provider == "none" || provider == "mock" { "— not configured" } else { "⚠  configured but not ready" };
        let _ = writeln!(e, "[{name}] {label}");
        let _ = writeln!(e, "       current: provider={} {}", if provider.is_empty() { "(none)" } else { provider }, badge);
        let _ = write!(e, "  Configure {name}? (y/N): ");
        let _ = e.flush();
        let yn = read_line()?.trim().to_ascii_lowercase();
        if matches!(yn.as_str(), "y" | "yes") {
            let res = match modality {
                Modality::Llm => wizard_llm(verify_after),
                Modality::Tts => wizard_media(media::tts_spec(), verify_after),
                Modality::Stt => wizard_media(media::stt_spec(), verify_after),
                Modality::ImageGen => wizard_media(media::imagegen_spec(), verify_after),
                Modality::Embed => wizard_media(media::embed_spec(), verify_after),
                _ => unreachable!(),
            };
            results.insert(name.into(), res.unwrap_or_else(|err| json!({"error": err})));
        } else {
            let _ = writeln!(e, "  → skipped");
            results.insert(name.into(), json!({"skipped": true}));
        }
        let _ = writeln!(e);
    }

    Ok(json!({
        "ok": true,
        "modalities": results,
        "next": "cos agent setup all --status",
    }))
}

/// Public façade over per-modality status — used by `cos agent doctor`
/// (and any other caller that wants a uniform snapshot without going
/// through the CLI surface).
pub fn status_for(modality: Modality) -> Value {
    match modality {
        Modality::Llm => status_llm(),
        Modality::All => {
            let mut map = serde_json::Map::new();
            map.insert("llm".into(), status_llm());
            for m in [Modality::Tts, Modality::Stt, Modality::ImageGen, Modality::Embed] {
                map.insert(m.name().into(), status_media(m));
            }
            json!({"modalities": map})
        }
        other => status_media(other),
    }
}

fn status_media(modality: Modality) -> Value {
    let spec = match modality {
        Modality::Tts => media::tts_spec(),
        Modality::Stt => media::stt_spec(),
        Modality::ImageGen => media::imagegen_spec(),
        Modality::Embed => media::embed_spec(),
        _ => return json!({"error": "not a media modality"}),
    };
    let snap = media::snapshot(spec.config_block);
    let provider = snap.get("provider").and_then(|s| s.as_str()).unwrap_or("none").to_string();
    let model = snap.get("model").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let credential = snap.get("api_key_credential").and_then(|s| s.as_str()).map(|s| s.to_string());
    let env = snap.get("api_key_env").and_then(|s| s.as_str()).map(|s| s.to_string());

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
        std::env::var(e).map(|s| !s.trim().is_empty()).unwrap_or(false)
    } else {
        false
    };
    let ready = provider != "none" && !provider.is_empty() && key_resolvable;

    let reason = if provider == "none" || provider.is_empty() {
        Some(format!("`{}` not configured (provider=none)", spec.name))
    } else if !key_resolvable {
        Some(format!(
            "provider `{provider}` needs a credential; none resolvable from store or env"
        ))
    } else {
        None
    };

    json!({
        "modality": spec.name,
        "config_block": spec.config_block,
        "ready": ready,
        "provider": provider,
        "model": model,
        "api_key_credential": credential,
        "api_key_env": env,
        "reason": reason,
    })
}

/// Reset a single media modality's config block to `provider=none`.
fn reset_media(modality: Modality) -> Result<Value, String> {
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
    let block = root.entry(block_name.to_string()).or_insert_with(|| json!({}));
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
fn verify_media(modality: Modality) -> Value {
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

fn fmt_ctx_window(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        format!("{tokens}")
    }
}

fn default_env_name(provider: &str) -> String {
    match provider {
        "anthropic" => "ANTHROPIC_API_KEY".into(),
        "openai" => "OPENAI_API_KEY".into(),
        "openai_compat" => "OPENAI_API_KEY".into(),
        "gemini" => "GEMINI_API_KEY".into(),
        "bedrock" => "AWS_BEARER_TOKEN_BEDROCK".into(),
        "xai" => "XAI_API_KEY".into(),
        "deepseek" => "DEEPSEEK_API_KEY".into(),
        "openrouter" => "OPENROUTER_API_KEY".into(),
        "ollama" => "OLLAMA_API_KEY".into(),
        other => format!("{}_API_KEY", other.to_uppercase()),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("read stdin: {e}"))?;
    Ok(line)
}

pub fn config_path() -> PathBuf {
    std::env::var("COS_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/cos/config.json"))
}

fn read_config_or_empty(path: &Path) -> Result<Value, String> {
    if !path.is_file() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn write_config_atomic(path: &Path, cfg: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}\nhint: try `sudo cos agent setup`", parent.display()))?;
    }
    let json_text =
        serde_json::to_string_pretty(cfg).map_err(|e| format!("serialize config: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json_text)
        .map_err(|e| format!("write {}: {e}\nhint: try `sudo cos agent setup`", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

fn store_credential(name: &str, value: &str) -> Result<(), String> {
    let args = vec![
        name.to_string(),
        value.to_string(),
        "--namespace".to_string(),
        "agent".to_string(),
    ];
    crate::credential::run("store", &args).map(|_| ())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_cfg() -> crate::config::AgentConfig {
        crate::config::AgentConfig::default()
    }

    // The wizard tests below mutate the process-wide COS_CONFIG_PATH
    // env var and write to that path. Cargo runs tests in parallel, so
    // we serialize them through a static mutex to avoid races.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn is_ready_blocks_on_mock_provider() {
        let err = is_ready(&mock_cfg()).unwrap_err();
        assert!(err.contains("agent not configured"));
        assert!(err.contains("cos agent setup"));
    }

    #[test]
    fn is_ready_passes_for_llama_local_without_credential() {
        let mut cfg = mock_cfg();
        cfg.provider = "llama_local".into();
        assert!(is_ready(&cfg).is_ok());
    }

    #[test]
    fn is_ready_blocks_real_provider_with_no_credential() {
        let mut cfg = mock_cfg();
        cfg.provider = "anthropic".into();
        cfg.model = "claude-3-5-sonnet".into();
        // Ensure no env var fallback can rescue us in CI:
        cfg.api_key_credential = Some("definitely_not_present".into());
        cfg.api_key_env = Some("__COS_TEST_DEFINITELY_UNSET__".into());
        std::env::remove_var("__COS_TEST_DEFINITELY_UNSET__");
        let err = is_ready(&cfg).unwrap_err();
        assert!(err.contains("no credential found"));
        assert!(err.contains("cos agent setup"));
    }

    #[test]
    fn is_ready_passes_when_env_credential_present() {
        let mut cfg = mock_cfg();
        cfg.provider = "anthropic".into();
        let env_name = "__COS_TEST_API_KEY_PRESENT__";
        cfg.api_key_env = Some(env_name.into());
        std::env::set_var(env_name, "sk-fake");
        assert!(is_ready(&cfg).is_ok());
        std::env::remove_var(env_name);
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        let err = run(&["bogus".into()]).unwrap_err();
        assert!(err.contains("unknown setup subcommand"), "got {err}");
    }

    #[test]
    fn unknown_flag_is_rejected() {
        let err = run(&["--bogus".into()]).unwrap_err();
        assert!(err.contains("unknown setup flag"), "got {err}");
    }

    #[test]
    fn help_subcommand_lists_modes() {
        let v = run(&["--help".into()]).expect("help ok");
        assert_eq!(
            v.get("command").and_then(|s| s.as_str()),
            Some("cos agent setup [MODALITY]")
        );
        let modalities = v.get("modalities").and_then(|s| s.as_object());
        assert!(modalities.is_some(), "expected modalities table in help");
        let m = modalities.unwrap();
        for k in ["llm", "tts", "stt", "imagegen", "embed", "all"] {
            assert!(m.contains_key(k), "expected modality `{k}` in help");
        }
    }

    #[test]
    fn modality_parses_aliases() {
        assert_eq!(Modality::parse("llm"), Some(Modality::Llm));
        assert_eq!(Modality::parse("speech"), Some(Modality::Tts));
        assert_eq!(Modality::parse("asr"), Some(Modality::Stt));
        assert_eq!(Modality::parse("image"), Some(Modality::ImageGen));
        assert_eq!(Modality::parse("embeddings"), Some(Modality::Embed));
        assert_eq!(Modality::parse("all"), Some(Modality::All));
        assert_eq!(Modality::parse("nope"), None);
    }

    #[test]
    fn reset_writes_mock_provider_to_config() {
        let _g = env_lock();
        use std::path::PathBuf;
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path: PathBuf = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);
        // Pre-seed with a non-mock provider.
        std::fs::write(
            &cfg_path,
            r#"{"agent":{"provider":"anthropic","model":"claude-3-5-sonnet"}}"#,
        )
        .unwrap();

        let v = reset_cmd(Modality::Llm).expect("reset ok");
        assert_eq!(v.get("ok").and_then(|b| b.as_bool()), Some(true));

        let text = std::fs::read_to_string(&cfg_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["agent"]["provider"].as_str(),
            Some("mock"),
            "expected reset to write provider=mock, got {text}"
        );

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn reset_media_writes_none_provider_to_config() {
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);
        std::fs::write(
            &cfg_path,
            r#"{"tts":{"provider":"openai","model":"tts-1","api_key_credential":"tts_openai_api_key"}}"#,
        )
        .unwrap();

        let v = reset_cmd(Modality::Tts).expect("reset ok");
        assert_eq!(v.get("ok").and_then(|b| b.as_bool()), Some(true));

        let text = std::fs::read_to_string(&cfg_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["tts"]["provider"].as_str(), Some("none"));
        assert!(parsed["tts"]["api_key_credential"].is_null());

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn status_returns_provider_and_ready_flag() {
        let _g = env_lock();
        // Use a tmp config path so we don't depend on /etc/cos/config.json.
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        let v = status_cmd(Modality::Llm).expect("status ok");
        assert!(v.get("ready").and_then(|b| b.as_bool()).is_some());
        assert!(v.get("provider").and_then(|s| s.as_str()).is_some());

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn status_all_reports_every_modality() {
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        let v = status_cmd(Modality::All).expect("status ok");
        let modalities = v.get("modalities").and_then(|s| s.as_object()).expect("modalities map");
        for k in ["llm", "tts", "stt", "imagegen", "embed"] {
            assert!(modalities.contains_key(k), "missing modality `{k}` in status");
        }

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}

//! `cos agent setup` — per-modality config wizard.
//!
//! Replaces the previous `cos agent onboarding` family. Pick a
//! modality (llm / tts / stt / imagegen / embed / all), then walk
//! through provider → model → API key → persist → optional probe.
//! Each modality writes to its own `/etc/cos/config.json` block
//! (`[agent]`, `[tts]`, `[stt]`, `[imagegen]`, `[embed]`) and stores
//! credentials in the `agent` namespace of the credential store.
//!
//! Subcommands:
//!   * `<modality>`     Run the wizard for one modality. Requires a TTY.
//!   * `all`            Walk every modality, asking before each.
//!   * `(no args)`      Interactive modality picker on TTY; help on non-TTY.
//!   * `--status`       Read-only: is the picked modality configured and
//!                      its credential resolvable? Defaults to `all` if
//!                      no modality is given.
//!   * `--reset`        Revert the picked modality's config block to its
//!                      built-in defaults. Defaults to `all` if no
//!                      modality is given.
//!   * `--verify-only`  Probe an already-persisted config without
//!                      re-running the wizard.
//!
//! The source of truth for "is X ready?" is always the live config +
//! credential store (`is_ready` for LLM, `status_for` for any modality),
//! never a separate state file.

use serde_json::{json, Value};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use crate::agent::llm;

pub fn run(args: &[String]) -> Result<Value, String> {
    // Parse: optional --no-verify / --verify-only / --status / --reset /
    // --providers / --help, plus a required leading positional modality:
    //   llm | tts | stt | imagegen | embed | all
    //
    // Two extra non-interactive subcommands take per-modality flags:
    //   apply  --provider X --model Y [--api-key K | --api-key-stdin | --api-key-env E]
    //   test   (alias for --verify-only)
    //
    // Bare `cos agent setup` (no positional, no flags) opens an
    // interactive modality picker on a TTY and prints help otherwise —
    // we deliberately do NOT auto-pick `llm` for the user.
    let mut verify_after = true;
    let mut explicit_verify = false;
    let mut sub: Option<&str> = None;
    let mut modality: Option<Modality> = None;
    let mut apply_provider: Option<String> = None;
    let mut apply_model: Option<String> = None;
    let mut apply_api_key: Option<String> = None;
    let mut apply_api_key_stdin = false;
    let mut apply_api_key_env: Option<String> = None;
    let mut apply_base_url: Option<String> = None;
    let mut apply_api_version: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--no-verify" => verify_after = false,
            "--verify" => verify_after = true,
            "--verify-only" => explicit_verify = true,
            "--status" | "status" if sub.is_none() => sub = Some("status"),
            "--reset" | "reset" if sub.is_none() => sub = Some("reset"),
            "--providers" | "providers" if sub.is_none() => sub = Some("providers"),
            "apply" if sub.is_none() => sub = Some("apply"),
            "test" if sub.is_none() => sub = Some("test"),
            "-h" | "--help" if sub.is_none() => sub = Some("help"),
            "--provider" => {
                i += 1;
                if i >= args.len() {
                    return Err("--provider requires a value".into());
                }
                apply_provider = Some(args[i].clone());
            }
            "--model" => {
                i += 1;
                if i >= args.len() {
                    return Err("--model requires a value".into());
                }
                apply_model = Some(args[i].clone());
            }
            "--api-key" => {
                i += 1;
                if i >= args.len() {
                    return Err("--api-key requires a value".into());
                }
                apply_api_key = Some(args[i].clone());
            }
            "--api-key-stdin" => apply_api_key_stdin = true,
            "--api-key-env" => {
                i += 1;
                if i >= args.len() {
                    return Err("--api-key-env requires a value".into());
                }
                apply_api_key_env = Some(args[i].clone());
            }
            "--base-url" => {
                i += 1;
                if i >= args.len() {
                    return Err("--base-url requires a value".into());
                }
                apply_base_url = Some(args[i].clone());
            }
            "--api-version" => {
                i += 1;
                if i >= args.len() {
                    return Err("--api-version requires a value".into());
                }
                apply_api_version = Some(args[i].clone());
            }
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
                        "unknown setup flag: {other}. try: --no-verify | --verify-only | --status | --reset | --providers"
                    ));
                } else if sub.is_none() {
                    return Err(format!(
                        "unknown setup modality/subcommand: {other}. try: llm | tts | stt | imagegen | embed | all | apply | test | --status | --reset | --providers | --verify-only"
                    ));
                }
            }
        }
        i += 1;
    }

    let apply_flags_set = apply_provider.is_some()
        || apply_model.is_some()
        || apply_api_key.is_some()
        || apply_api_key_stdin
        || apply_api_key_env.is_some()
        || apply_base_url.is_some()
        || apply_api_version.is_some();
    if apply_flags_set && sub != Some("apply") {
        return Err(
            "--provider / --model / --api-key{,-stdin,-env} / --base-url / --api-version are only valid with the `apply` subcommand"
                .into(),
        );
    }

    let Some(modality) = modality else {
        // No positional given. For sub-only invocations (--status /
        // --reset / --verify-only / --providers / --help) treat that as
        // "all" so the user gets a uniform multi-modality view. For the
        // bare wizard we refuse to guess.
        if sub == Some("help") {
            return Ok(help_doc());
        }
        if explicit_verify {
            return verify_cmd(Modality::All);
        }
        return match sub {
            Some("status") => status_cmd(Modality::All),
            Some("reset") => reset_cmd(Modality::All),
            Some("providers") => providers_cmd(Modality::All),
            Some("apply") => Err(
                "`apply` requires a modality: cos agent setup <llm|tts|stt|imagegen|embed> apply ..."
                    .into(),
            ),
            Some("test") => verify_cmd(Modality::All),
            _ => {
                if std::io::stdin().is_terminal() {
                    let picked = pick_modality_interactively()?;
                    dispatch_wizard(picked, verify_after)
                } else {
                    Err(json!({
                        "error": "cos agent setup requires a modality",
                        "hint": "pick one of: llm | tts | stt | imagegen | embed | all",
                        "examples": [
                            "cos agent setup llm",
                            "cos agent setup tts",
                            "cos agent setup all",
                        ],
                    })
                    .to_string())
                }
            }
        };
    };

    if explicit_verify {
        return verify_cmd(modality);
    }
    let apply_args = ApplyArgs {
        provider: apply_provider,
        model: apply_model,
        api_key: apply_api_key,
        api_key_stdin: apply_api_key_stdin,
        api_key_env: apply_api_key_env,
        base_url: apply_base_url,
        api_version: apply_api_version,
    };
    match sub {
        Some("status") => status_cmd(modality),
        Some("reset") => reset_cmd(modality),
        Some("providers") => providers_cmd(modality),
        Some("apply") => apply_cmd(modality, apply_args),
        Some("test") => verify_cmd(modality),
        Some("help") => Ok(help_doc()),
        _ => dispatch_wizard(modality, verify_after),
    }
}

fn dispatch_wizard(modality: Modality, verify_after: bool) -> Result<Value, String> {
    match modality {
        Modality::Llm => wizard_llm(verify_after),
        Modality::Tts => wizard_media(media::tts_spec(), verify_after),
        Modality::Stt => wizard_media(media::stt_spec(), verify_after),
        Modality::ImageGen => wizard_media(media::imagegen_spec(), verify_after),
        Modality::Embed => wizard_media(media::embed_spec(), verify_after),
        Modality::All => wizard_all(verify_after),
    }
}

fn pick_modality_interactively() -> Result<Modality, String> {
    let stderr = std::io::stderr();
    let mut e = stderr.lock();
    let _ = writeln!(e, "cos agent setup — pick a modality to configure");
    let _ = writeln!(e);
    let options = [
        (Modality::Llm,      "llm",      "Conversational LLM (required for ask/chat)"),
        (Modality::Tts,      "tts",      "Text-to-speech"),
        (Modality::Stt,      "stt",      "Speech-to-text"),
        (Modality::ImageGen, "imagegen", "Image generation"),
        (Modality::Embed,    "embed",    "Text embeddings (semantic memory)"),
        (Modality::All,      "all",      "Walk every modality, asking before each"),
    ];
    for (i, (_, name, label)) in options.iter().enumerate() {
        let _ = writeln!(e, "  {}. {:8} — {}", i + 1, name, label);
    }
    let _ = write!(e, "Pick one (1-{}): ", options.len());
    let _ = e.flush();
    let raw = read_line()?.trim().to_string();
    let idx: usize = raw.parse().map_err(|_| "expected a number".to_string())?;
    if idx < 1 || idx > options.len() {
        return Err(format!("out of range: {idx} (expected 1-{})", options.len()));
    }
    Ok(options[idx - 1].0)
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
        "command": "cos agent setup <MODALITY> [SUBCOMMAND]",
        "summary": "Per-modality config wizard: pick a provider, a model, store an API key, and verify it works.",
        "modalities": {
            "llm":       "Conversational LLM (under [agent]). Required for `cos agent ask`/`chat`.",
            "tts":       "Text-to-speech (under [tts]). Used by voice output / `cos agent voice`.",
            "stt":       "Speech-to-text (under [stt]). Used by voice input / `cos agent transcribe`.",
            "imagegen":  "Image generation (under [imagegen]). Used by `cos agent image`.",
            "embed":     "Text embeddings (under [embed]). Used by semantic memory / `cos agent recall`.",
            "all":       "Walk every modality in order, prompting before each.",
        },
        "subcommands": {
            "apply":     "Non-interactive write. Required flags: --provider X --model Y. Credential: one of --api-key K | --api-key-stdin | --api-key-env ENV. `--provider none` clears the modality.",
            "test":      "Alias for --verify-only: probe the currently configured provider for this modality.",
            "providers": "Emit JSON catalogue of providers + sample models for the picked modality (or `all`). Used by the cosmic-settings agent page.",
        },
        "flags": {
            "--no-verify":      "Skip the live provider probe at the end of the wizard.",
            "--verify-only":    "Skip the wizard; just probe the currently configured provider for the given modality (or all if omitted).",
            "--status":         "Show whether the picked modality is configured. With no modality (or `all`), shows every modality.",
            "--reset":          "Revert the picked modality's config block to its built-in defaults (`mock`/`none`). With no modality, resets every modality.",
            "--providers":      "Same as the `providers` subcommand.",
            "--provider X":     "(apply only) Provider name. Use `none` to clear the modality.",
            "--model Y":        "(apply only) Model name.",
            "--api-key VALUE":  "(apply only) Inline API key. Stored in the agent credential namespace.",
            "--api-key-stdin":  "(apply only) Read API key from stdin instead of the command line — preferred to keep keys out of shell history.",
            "--api-key-env E":  "(apply only) Don't store a key; persist a pointer to env var `$E`.",
            "--base-url URL":   "(apply only) Override the provider's default API endpoint. REQUIRED when --provider azure (Azure has no universal default — point at https://<resource>.openai.azure.com/openai/deployments/<deployment>). Accepted as an advanced override for openai / xai / deepseek / openrouter / ollama.",
            "--api-version V":  "(apply only) Azure REST API version, e.g. 2024-12-01-preview. When set and --base-url has no `?`, gets appended as `?api-version=V`.",
        },
        "examples": [
            "cos agent setup llm                                                  # wizard for LLM",
            "cos agent setup all                                                  # walk every modality, asking before each",
            "cos agent setup --status                                             # report readiness across all modalities",
            "cos agent setup --providers                                          # JSON catalogue of all providers + models",
            "cos agent setup llm apply --provider openai --model gpt-4o --api-key-stdin  < key.txt",
            "cos agent setup llm apply --provider azure --model my-deployment \\",
            "    --base-url https://acme.openai.azure.com/openai/deployments/my-deployment \\",
            "    --api-version 2024-12-01-preview --api-key-stdin                # Azure OpenAI",
            "cos agent setup tts apply --provider edge --model en-US-AriaNeural   # no key needed",
            "cos agent setup imagegen test                                        # probe configured imagegen provider",
        ],
        "notes": [
            "Bare `cos agent setup` (no args) opens an interactive picker on a TTY; on a non-TTY it errors and lists the modalities.",
            "All subcommands emit JSON on success; errors are plain strings on stderr.",
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
            "hint": "run `cos agent setup llm` to configure a provider first",
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
/// `cos agent setup llm` when the agent is still on the mock provider or
/// the configured provider has no resolvable credential.
pub fn is_ready(cfg: &crate::config::AgentConfig) -> Result<(), String> {
    if cfg.provider == "mock" {
        return Err(json!({
            "error": "agent not configured",
            "fix": "cos agent setup llm",
            "details": "the default provider is `mock` (returns canned answers). Run `cos agent setup llm` to pick a real LLM provider.",
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
        "fix": "cos agent setup llm",
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
    let (base_url, api_version) = split_base_url_and_api_version(cfg.base_url.as_deref());
    json!({
        "modality": "llm",
        "ready": ready.is_ok(),
        "provider": cfg.provider,
        "model": cfg.model,
        "api_key_credential": cfg.api_key_credential,
        "api_key_env": cfg.api_key_env,
        "base_url": cfg.base_url,
        "endpoint": base_url,
        "api_version": api_version,
        "config_path": config_path().display().to_string(),
        "reason": reason,
    })
}

/// Split a stored `base_url` into its constituent parts so the UI can
/// pre-fill separate "endpoint" / "API version" inputs without having
/// to re-parse query strings itself.
///
/// Returns `(endpoint_without_query, api_version_value)`. Either side
/// may be `None`: a `base_url` of `None` produces `(None, None)`; a
/// `base_url` without a `?api-version=` returns the original URL as
/// endpoint and `None` for the version.
fn split_base_url_and_api_version(base_url: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(raw) = base_url else {
        return (None, None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let (head, query) = match trimmed.split_once('?') {
        Some((h, q)) => (h.to_string(), Some(q)),
        None => (trimmed.to_string(), None),
    };
    let api_version = query.and_then(|q| {
        q.split('&').find_map(|pair| {
            let mut it = pair.splitn(2, '=');
            match (it.next(), it.next()) {
                (Some(k), Some(v)) if k.eq_ignore_ascii_case("api-version") && !v.is_empty() => {
                    Some(v.to_string())
                }
                _ => None,
            }
        })
    });
    (Some(head), api_version)
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
    let _ = writeln!(e, "cos agent setup llm — conversational LLM wizard");
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

    // ---- Step 1b: Azure-only — endpoint + api version ------------------
    //
    // Azure has no universal default base URL — every deployment lives
    // at `https://<resource>.openai.azure.com/openai/deployments/<dep>`
    // and the REST API requires `?api-version=<version>`. Prompt up
    // front so the model picker can advise that the model name should
    // match the deployment name.
    let mut azure_base_url: Option<String> = None;
    let mut azure_api_version: Option<String> = None;
    if provider == "azure" {
        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "Azure OpenAI: paste the resource root URL from your Azure portal."
        );
        let _ = writeln!(e, "(e.g. https://acme.openai.azure.com/)");
        let _ = writeln!(
            e,
            "Do NOT include /openai/deployments/...  — that path is added"
        );
        let _ = writeln!(e, "automatically using the model name you supply below.");
        let _ = write!(e, "Endpoint: ");
        let _ = e.flush();
        let url = read_line()?.trim().to_string();
        if url.is_empty() {
            return Err("azure endpoint cannot be empty".into());
        }
        let lower = url.to_ascii_lowercase();
        if lower.contains("/openai/deployments/") || lower.contains("/openai/responses") {
            return Err(
                "that looks like a full deployment URL — paste the resource root only \
                 (e.g. https://<resource>.openai.azure.com/). The deployment path is \
                 added automatically from the model name."
                    .into(),
            );
        }
        azure_base_url = Some(url);

        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "API version (e.g. 2024-12-01-preview). Press Enter to skip"
        );
        let _ = writeln!(
            e,
            "if your endpoint URL already includes ?api-version=…"
        );
        let _ = write!(e, "Version: ");
        let _ = e.flush();
        let v = read_line()?.trim().to_string();
        if !v.is_empty() {
            azure_api_version = Some(v);
        }
        let _ = writeln!(e);
        let _ = writeln!(
            e,
            "Note: the model name below must match the deployment name in Azure."
        );
    }

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
                    "a privileged session (e.g. `sudo COS_CONFIG_PATH=$COS_CONFIG_PATH cos agent setup llm`)."
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

    // Azure resource-root URL (+ optional --api-version glue),
    // normalised through the same helper the non-interactive `apply`
    // uses so the on-disk shape is identical no matter which entry
    // point persisted the config. The kernel's openai_compat provider
    // composes the full `/openai/deployments/<dep>/chat/completions`
    // path itself using the `model` field as the deployment name.
    if provider == "azure" {
        let resolved = resolve_base_url_args(
            &provider,
            azure_base_url.as_deref(),
            azure_api_version.as_deref(),
        )?;
        apply_base_url_to_block(agent, resolved.as_deref());
    } else {
        // Non-azure providers keep any previously-set override
        // untouched here — the wizard doesn't ask about base_url for
        // them. Users wanting an override should use the `apply`
        // subcommand or edit the config file directly.
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
            "(skipping live probe; re-run with `cos agent setup llm --verify-only` to confirm later)"
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
                "  hint: fix the credential, then run `cos agent setup llm --verify-only` to retest (no re-wizard)."
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
    let raw_provider = snap.get("provider").and_then(|s| s.as_str()).unwrap_or("").to_string();
    // For `embed`, the serde default is `"auto"` (derive the embedder
    // from the main `[agent]` provider). An empty / missing on-disk
    // value is therefore equivalent to `"auto"`, not `"none"`.
    let provider = if matches!(modality, Modality::Embed) && raw_provider.is_empty() {
        crate::config::get().embed.provider.clone()
    } else if raw_provider.is_empty() {
        "none".to_string()
    } else {
        raw_provider
    };
    let model = snap.get("model").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let credential = snap.get("api_key_credential").and_then(|s| s.as_str()).map(|s| s.to_string());
    let env = snap.get("api_key_env").and_then(|s| s.as_str()).map(|s| s.to_string());
    let base_url_raw = snap.get("base_url").and_then(|s| s.as_str()).map(|s| s.to_string());
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
        std::env::var(e).map(|s| !s.trim().is_empty()).unwrap_or(false)
    } else {
        false
    };
    // `embed` has a special `"auto"` provider that derives the
    // embedder from the main `[agent]` config. It's ready iff the
    // derivation actually produces an embedder (the main provider
    // is OpenAI-shape).
    let (ready, reason) = if matches!(modality, Modality::Embed) && provider == "auto" {
        let derived = crate::model::tasks::embed::build_default().ok().flatten();
        match derived {
            Some(_) => (
                true,
                Some(format!(
                    "auto-derived from main agent provider `{}`",
                    crate::config::get().agent.provider
                )),
            ),
            None => (
                false,
                Some(format!(
                    "`embed.provider=auto` but main agent provider `{}` does not support \
                     auto-derivation — set `[embed]` explicitly to enable",
                    crate::config::get().agent.provider
                )),
            ),
        }
    } else {
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
        "azure" => "AZURE_OPENAI_API_KEY".into(),
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
// Non-interactive subcommands (used by cosmic-settings and other UIs)
// ---------------------------------------------------------------------------

/// Args bundle for the `apply` subcommand. Mirrors the wizard's persistence
/// step but driven entirely from flags, never stdin (except `--api-key-stdin`).
struct ApplyArgs {
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    api_key_stdin: bool,
    api_key_env: Option<String>,
    /// Optional custom endpoint. Required for `azure`; accepted as an
    /// advanced override for the other `openai_compat`-family aliases.
    base_url: Option<String>,
    /// Azure REST API version. When set and `base_url` lacks a query
    /// string, gets appended as `?api-version=<value>` at persist time.
    api_version: Option<String>,
}

/// Emit a JSON catalogue of providers + sample models for one or all
/// modalities. The cosmic-settings agent page uses this to populate
/// provider dropdowns, model suggestions, and "needs API key" badges
/// without hard-coding any of it.
fn providers_cmd(modality: Modality) -> Result<Value, String> {
    match modality {
        Modality::Llm => Ok(providers_llm()),
        Modality::All => {
            let mut m = serde_json::Map::new();
            m.insert("llm".into(), providers_llm());
            for md in [Modality::Tts, Modality::Stt, Modality::ImageGen, Modality::Embed] {
                m.insert(md.name().into(), providers_media(md));
            }
            Ok(json!({"modalities": m}))
        }
        other => Ok(providers_media(other)),
    }
}

fn providers_llm() -> Value {
    let names = llm::available_providers();
    let providers: Vec<Value> = names
        .iter()
        .map(|name| {
            let models = llm::metadata::list_for_provider(name);
            let model_list: Vec<Value> = models
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "context_window": m.context_window,
                        "max_output_tokens": m.max_output_tokens,
                        "supports_tools": m.supports_tools,
                        "supports_vision": m.supports_vision,
                        "supports_streaming": m.supports_streaming,
                    })
                })
                .collect();
            json!({
                "name": name,
                "label": *name,
                "needs_credential": provider_needs_credential(name),
                "default_env": default_env_name(name),
                "models": model_list,
                "default_model": models.first().map(|m| m.name.to_string()).unwrap_or_default(),
                "extra_fields": extra_fields_for(name),
            })
        })
        .collect();
    json!({
        "modality": "llm",
        "providers": providers,
    })
}

fn providers_media(modality: Modality) -> Value {
    let spec = match modality {
        Modality::Tts => media::tts_spec(),
        Modality::Stt => media::stt_spec(),
        Modality::ImageGen => media::imagegen_spec(),
        Modality::Embed => media::embed_spec(),
        _ => return json!({"error": "not a media modality"}),
    };
    let provs: Vec<Value> = spec
        .providers
        .iter()
        .map(|p| {
            let models: Vec<Value> = p
                .sample_models
                .iter()
                .map(|m| json!({ "name": m }))
                .collect();
            json!({
                "name": p.name,
                "label": p.label,
                "needs_credential": p.needs_credential,
                "default_env": p.default_env,
                "models": models,
                "default_model": p.default_model,
                "extra_fields": extra_fields_for(p.name),
            })
        })
        .collect();
    json!({
        "modality": spec.name,
        "default_provider": spec.default_provider,
        "providers": provs,
    })
}

/// Per-provider declarative form schema. Empty for providers that need
/// only model + credential; non-empty for providers like `azure` that
/// require additional inputs (endpoint URL, API version, …). UIs walk
/// this list and render appropriate inputs without hard-coding any
/// per-provider rules.
fn extra_fields_for(provider: &str) -> Vec<Value> {
    match provider {
        "azure" => vec![
            json!({
                "key": "base_url",
                "label": "Azure endpoint",
                "placeholder": "https://<resource>.openai.azure.com/",
                "help": "The resource root URL from your Azure OpenAI portal. Do NOT include /openai/deployments/… — that path is constructed automatically using the model field (which must match your Azure deployment name).",
                "required": true,
                "secret": false,
            }),
            json!({
                "key": "api_version",
                "label": "API version",
                "placeholder": "2024-12-01-preview",
                "help": "Azure REST API version. Find current versions in the Azure OpenAI docs.",
                "required": false,
                "secret": false,
            }),
        ],
        _ => Vec::new(),
    }
}

/// Non-interactive write. Validates inputs against the same catalogues
/// the wizard uses, stores credentials in the agent namespace, and
/// writes the modality's config block atomically. Returns a JSON
/// envelope the UI can render directly.
fn apply_cmd(modality: Modality, args: ApplyArgs) -> Result<Value, String> {
    match modality {
        Modality::Llm => apply_llm(args),
        Modality::All => Err(
            "`apply` requires a single modality (one of: llm | tts | stt | imagegen | embed)"
                .into(),
        ),
        Modality::Tts => apply_media(media::tts_spec(), args),
        Modality::Stt => apply_media(media::stt_spec(), args),
        Modality::ImageGen => apply_media(media::imagegen_spec(), args),
        Modality::Embed => apply_media(media::embed_spec(), args),
    }
}

fn apply_llm(args: ApplyArgs) -> Result<Value, String> {
    let provider = args
        .provider
        .as_deref()
        .ok_or_else(|| "--provider is required for `apply`".to_string())?
        .trim()
        .to_string();
    if provider.is_empty() {
        return Err("--provider cannot be empty".into());
    }
    if provider == "none" {
        return reset_cmd(Modality::Llm);
    }
    let available = llm::available_providers();
    if !available.iter().any(|p| *p == provider) {
        return Err(format!(
            "unknown LLM provider `{provider}`. available providers in this build: {}",
            available.join(", ")
        ));
    }

    let model = args
        .model
        .as_deref()
        .ok_or_else(|| "--model is required for `apply`".to_string())?
        .trim()
        .to_string();
    if model.is_empty() {
        return Err("--model cannot be empty".into());
    }

    let resolved_base_url =
        resolve_base_url_args(&provider, args.base_url.as_deref(), args.api_version.as_deref())?;

    let needs_cred = provider_needs_credential(&provider);
    let credential_hint = format!("{provider}_api_key");
    let (credential_name, credential_env) =
        resolve_key_args(&args, &provider, &credential_hint, needs_cred)?;

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
    apply_credential_to_block(agent, credential_name.as_deref(), credential_env.as_deref());
    apply_base_url_to_block(agent, resolved_base_url.as_deref());
    write_config_atomic(&path, &cfg)?;

    Ok(json!({
        "ok": true,
        "modality": "llm",
        "provider": provider,
        "model": model,
        "api_key_credential": credential_name,
        "api_key_env": credential_env,
        "key_source": key_source_label(credential_name.as_deref(), credential_env.as_deref(), needs_cred),
        "base_url": resolved_base_url,
        "config_path": path.display().to_string(),
    }))
}

fn apply_media(
    spec: &'static media::ModalitySpec,
    args: ApplyArgs,
) -> Result<Value, String> {
    let provider_name = args
        .provider
        .as_deref()
        .ok_or_else(|| "--provider is required for `apply`".to_string())?
        .trim()
        .to_string();
    if provider_name.is_empty() {
        return Err("--provider cannot be empty".into());
    }
    if provider_name == "none" {
        return reset_cmd(match spec.name {
            "tts" => Modality::Tts,
            "stt" => Modality::Stt,
            "imagegen" => Modality::ImageGen,
            "embed" => Modality::Embed,
            _ => unreachable!("unknown media spec: {}", spec.name),
        });
    }
    let provider = spec.provider_choice(&provider_name).ok_or_else(|| {
        let known: Vec<&str> = spec.providers.iter().map(|p| p.name).collect();
        format!(
            "unknown `{}` provider `{provider_name}`. known: {} (or `none` to clear)",
            spec.name,
            known.join(", ")
        )
    })?;

    let model = args
        .model
        .as_deref()
        .ok_or_else(|| "--model is required for `apply`".to_string())?
        .trim()
        .to_string();
    if model.is_empty() {
        return Err("--model cannot be empty".into());
    }

    let resolved_base_url = resolve_base_url_args(
        provider.name,
        args.base_url.as_deref(),
        args.api_version.as_deref(),
    )?;

    let credential_hint = format!("{}_{}_api_key", spec.name, provider.name);
    let (credential_name, credential_env) = resolve_key_args(
        &args,
        provider.name,
        &credential_hint,
        provider.needs_credential,
    )?;

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
    apply_credential_to_block(block, credential_name.as_deref(), credential_env.as_deref());
    apply_base_url_to_block(block, resolved_base_url.as_deref());
    write_config_atomic(&path, &cfg)?;

    Ok(json!({
        "ok": true,
        "modality": spec.name,
        "provider": provider.name,
        "model": model,
        "api_key_credential": credential_name,
        "api_key_env": credential_env,
        "key_source": key_source_label(
            credential_name.as_deref(),
            credential_env.as_deref(),
            provider.needs_credential,
        ),
        "base_url": resolved_base_url,
        "config_path": path.display().to_string(),
    }))
}

/// Resolve the three mutually-exclusive credential flags into a
/// `(credential_name, env_name)` pair matching the wizard's persistence
/// shape. Returns `(None, None)` when the provider does not need a key.
fn resolve_key_args(
    args: &ApplyArgs,
    provider_name: &str,
    credential_hint: &str,
    needs_credential: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let supplied = [
        args.api_key.is_some(),
        args.api_key_stdin,
        args.api_key_env.is_some(),
    ];
    let count: u8 = supplied.iter().map(|b| *b as u8).sum();
    if count > 1 {
        return Err(
            "specify at most one of --api-key / --api-key-stdin / --api-key-env".into(),
        );
    }
    if !needs_credential {
        if count > 0 {
            return Err(format!(
                "provider `{provider_name}` does not need an API key; drop --api-key{{,-stdin,-env}}"
            ));
        }
        return Ok((None, None));
    }
    if let Some(env) = args.api_key_env.as_deref() {
        let env = env.trim();
        if env.is_empty() {
            return Err("--api-key-env cannot be empty".into());
        }
        return Ok((None, Some(env.to_string())));
    }
    let key: String = if args.api_key_stdin {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("read --api-key-stdin: {e}"))?;
        s.trim().to_string()
    } else if let Some(k) = args.api_key.as_deref() {
        k.trim().to_string()
    } else {
        return Err(format!(
            "provider `{provider_name}` needs a credential; pass --api-key VALUE, --api-key-stdin, or --api-key-env ENV_NAME"
        ));
    };
    if key.is_empty() {
        return Err("API key cannot be empty".into());
    }
    store_credential(credential_hint, &key).map_err(|e| {
        format!(
            "store credential `{credential_hint}` in namespace `agent`: {e}\nhint: run under a privileged shell, or use --api-key-env ENV_NAME to point at an env var instead"
        )
    })?;
    Ok((Some(credential_hint.to_string()), None))
}

fn apply_credential_to_block(
    block: &mut serde_json::Map<String, Value>,
    credential_name: Option<&str>,
    credential_env: Option<&str>,
) {
    if let Some(n) = credential_name {
        block.insert("api_key_credential".into(), json!(n));
        block.remove("api_key_env");
    } else if let Some(env) = credential_env {
        block.insert("api_key_env".into(), json!(env));
        block.insert("api_key_credential".into(), Value::Null);
    } else {
        block.insert("api_key_credential".into(), Value::Null);
        block.remove("api_key_env");
    }
}

/// Validate `--base-url` / `--api-version` for `apply` and return the
/// final string that should land in `block["base_url"]`. Returns
/// `Ok(None)` when neither flag was supplied for a non-azure provider
/// (preserving the prior config entry untouched is the caller's job).
///
/// Rules:
/// - Provider `azure` always requires `--base-url`.
/// - `--api-version` is only meaningful for `azure`; we accept it on
///   other providers as a passthrough convenience and append it the same
///   way, but most users won't ever need it.
/// - If `--base-url` already contains a `?`, we leave the query alone
///   and ignore `--api-version` (with a soft warning baked into the
///   eventual error path — for now we just respect the explicit value).
fn resolve_base_url_args(
    provider_name: &str,
    base_url: Option<&str>,
    api_version: Option<&str>,
) -> Result<Option<String>, String> {
    let trimmed = base_url.map(str::trim).filter(|s| !s.is_empty());
    if provider_name == "azure" && trimmed.is_none() {
        return Err(
            "provider `azure` requires --base-url <RESOURCE_ROOT> \
             (e.g. https://<resource>.openai.azure.com/). The /openai/deployments/… \
             path is added automatically using the --model value as the deployment name."
                .into(),
        );
    }
    let Some(base) = trimmed else {
        return Ok(None);
    };

    // Common Azure footgun: the user grabs the "Target URI" from the
    // portal which already includes `/openai/deployments/<deployment>`
    // (and sometimes `/chat/completions` or `/responses`). Detect and
    // reject up front so it doesn't 404 later.
    if provider_name == "azure" {
        let lower = base.to_ascii_lowercase();
        if lower.contains("/openai/deployments/") || lower.contains("/openai/responses") {
            return Err(
                "azure --base-url should be the resource root \
                 (e.g. https://<resource>.openai.azure.com/), not the full \
                 deployment URL. Pass the deployment name via --model and the \
                 API version via --api-version."
                    .into(),
            );
        }
    }

    let av = api_version.map(str::trim).filter(|s| !s.is_empty());
    let already_has_query = base.contains('?');
    let final_url = match av {
        Some(v) if !already_has_query => format!("{base}?api-version={v}"),
        Some(_) | None => base.to_string(),
    };
    Ok(Some(final_url))
}

/// Persist `--base-url` decisions into the modality block. Always
/// produces a deterministic shape so a re-apply can clear a previously
/// stored value: explicitly setting `null` when the caller decided not
/// to override and the provider doesn't require it.
fn apply_base_url_to_block(
    block: &mut serde_json::Map<String, Value>,
    base_url: Option<&str>,
) {
    match base_url {
        Some(url) => {
            block.insert("base_url".into(), json!(url));
        }
        None => {
            // Drop any stale override so subsequent runs fall back to
            // the alias's default. Use `remove` rather than null so the
            // on-disk config stays minimal.
            block.remove("base_url");
        }
    }
}

fn key_source_label(
    credential_name: Option<&str>,
    credential_env: Option<&str>,
    needs_credential: bool,
) -> &'static str {
    if !needs_credential {
        "not-required"
    } else if credential_name.is_some() {
        "credential"
    } else if credential_env.is_some() {
        "env"
    } else {
        "none"
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
        assert!(err.contains("unknown setup modality/subcommand"), "got {err}");
    }

    #[test]
    fn bare_setup_on_non_tty_requires_modality() {
        // Cargo test may inherit a TTY stdin in interactive shells; in
        // that case `run` would block on the modality picker. Skip
        // unless stdin is actually piped (which it is in CI).
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            eprintln!("(skipping: stdin is a TTY in this test run)");
            return;
        }
        let err = run(&[]).unwrap_err();
        assert!(err.contains("requires a modality"), "got {err}");
        // The envelope should also list the valid modalities so callers
        // can self-correct.
        for m in ["llm", "tts", "stt", "imagegen", "embed", "all"] {
            assert!(err.contains(m), "expected `{m}` in envelope; got {err}");
        }
    }

    #[test]
    fn bare_status_defaults_to_all_modalities() {
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        // No positional modality, just --status: should fan out to all.
        let v = run(&["--status".into()]).expect("status ok");
        let modalities = v.get("modalities").and_then(|s| s.as_object()).expect("modalities map");
        for k in ["llm", "tts", "stt", "imagegen", "embed"] {
            assert!(modalities.contains_key(k), "missing modality `{k}` in bare status");
        }

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn unknown_flag_is_rejected() {
        let err = run(&["--bogus".into()]).unwrap_err();
        assert!(err.contains("unknown setup flag"), "got {err}");
    }

    #[test]
    fn help_subcommand_lists_modes() {
        let v = run(&["--help".into()]).expect("help ok");
        let cmd = v.get("command").and_then(|s| s.as_str()).unwrap_or("");
        assert!(
            cmd.starts_with("cos agent setup <MODALITY>"),
            "expected help command to start with `cos agent setup <MODALITY>`; got `{cmd}`"
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

    // ---- Azure first-class support --------------------------------------

    #[test]
    fn resolve_base_url_appends_api_version_when_missing_query() {
        let got = resolve_base_url_args(
            "azure",
            Some("https://acme.openai.azure.com/"),
            Some("2024-12-01-preview"),
        )
        .unwrap();
        assert_eq!(
            got.as_deref(),
            Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview")
        );
    }

    #[test]
    fn resolve_base_url_preserves_existing_query() {
        let got = resolve_base_url_args(
            "azure",
            Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview&foo=bar"),
            Some("ignored-because-base-already-has-query"),
        )
        .unwrap();
        assert_eq!(
            got.as_deref(),
            Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview&foo=bar")
        );
    }

    #[test]
    fn resolve_base_url_azure_rejects_full_deployment_url() {
        let err = resolve_base_url_args(
            "azure",
            Some("https://acme.openai.azure.com/openai/deployments/gpt-5.4"),
            Some("2024-12-01-preview"),
        )
        .unwrap_err();
        assert!(err.contains("resource root"), "msg was: {err}");
    }

    #[test]
    fn resolve_base_url_azure_rejects_responses_endpoint() {
        let err = resolve_base_url_args(
            "azure",
            Some("https://acme.openai.azure.com/openai/responses"),
            Some("2025-04-01-preview"),
        )
        .unwrap_err();
        assert!(err.contains("resource root"), "msg was: {err}");
    }

    #[test]
    fn resolve_base_url_azure_requires_base_url() {
        let err = resolve_base_url_args("azure", None, Some("2024-12-01-preview")).unwrap_err();
        assert!(err.contains("azure"), "{err}");
        assert!(err.contains("--base-url"), "{err}");
    }

    #[test]
    fn resolve_base_url_non_azure_accepts_no_override() {
        let got = resolve_base_url_args("openai", None, None).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn resolve_base_url_non_azure_accepts_override() {
        let got = resolve_base_url_args("openai", Some("https://my.proxy/v1"), None).unwrap();
        assert_eq!(got.as_deref(), Some("https://my.proxy/v1"));
    }

    #[test]
    fn split_base_url_parses_api_version_query() {
        let (endpoint, version) = split_base_url_and_api_version(Some(
            "https://acme.openai.azure.com/openai/deployments/dep?api-version=2024-12-01-preview",
        ));
        assert_eq!(
            endpoint.as_deref(),
            Some("https://acme.openai.azure.com/openai/deployments/dep")
        );
        assert_eq!(version.as_deref(), Some("2024-12-01-preview"));
    }

    #[test]
    fn split_base_url_handles_no_query() {
        let (endpoint, version) =
            split_base_url_and_api_version(Some("https://api.openai.com/v1"));
        assert_eq!(endpoint.as_deref(), Some("https://api.openai.com/v1"));
        assert!(version.is_none());
    }

    #[test]
    fn split_base_url_handles_none() {
        let (endpoint, version) = split_base_url_and_api_version(None);
        assert!(endpoint.is_none());
        assert!(version.is_none());
    }

    #[test]
    fn split_base_url_handles_empty_string() {
        let (endpoint, version) = split_base_url_and_api_version(Some(""));
        assert!(endpoint.is_none());
        assert!(version.is_none());
    }

    #[test]
    fn split_base_url_handles_query_without_api_version() {
        let (endpoint, version) =
            split_base_url_and_api_version(Some("https://api.openai.com/v1?foo=bar"));
        assert_eq!(endpoint.as_deref(), Some("https://api.openai.com/v1"));
        assert!(version.is_none());
    }

    #[test]
    fn extra_fields_for_azure_lists_endpoint_and_api_version() {
        let fields = extra_fields_for("azure");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0]["key"].as_str(), Some("base_url"));
        assert_eq!(fields[0]["required"].as_bool(), Some(true));
        assert_eq!(fields[1]["key"].as_str(), Some("api_version"));
        assert_eq!(fields[1]["required"].as_bool(), Some(false));
    }

    #[test]
    fn extra_fields_for_non_azure_is_empty() {
        assert!(extra_fields_for("openai").is_empty());
        assert!(extra_fields_for("anthropic").is_empty());
        assert!(extra_fields_for("mock").is_empty());
    }

    #[test]
    fn providers_cmd_llm_includes_azure_with_extra_fields() {
        let v = providers_cmd(Modality::Llm).expect("providers ok");
        let providers = v.get("providers").and_then(|p| p.as_array()).expect("providers list");
        let azure = providers
            .iter()
            .find(|p| p["name"] == "azure")
            .expect("azure provider entry");
        let extras = azure["extra_fields"].as_array().expect("extra_fields array");
        assert_eq!(extras.len(), 2);
        assert_eq!(extras[0]["key"], "base_url");
        assert_eq!(extras[1]["key"], "api_version");
        // Non-azure provider: extra_fields present but empty.
        let openai = providers
            .iter()
            .find(|p| p["name"] == "openai")
            .expect("openai provider entry");
        assert!(openai["extra_fields"].as_array().unwrap().is_empty());
    }

    #[test]
    fn azure_apply_without_base_url_errors() {
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        let err = run(&[
            "llm".into(),
            "apply".into(),
            "--provider".into(),
            "azure".into(),
            "--model".into(),
            "my-deployment".into(),
            "--api-key-env".into(),
            "__AZURE_TEST_KEY__".into(),
        ])
        .unwrap_err();
        assert!(err.contains("azure"), "{err}");
        assert!(err.contains("--base-url"), "{err}");

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn azure_apply_persists_base_url_with_api_version() {
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        let v = run(&[
            "llm".into(),
            "apply".into(),
            "--provider".into(),
            "azure".into(),
            "--model".into(),
            "my-deployment".into(),
            "--base-url".into(),
            "https://acme.openai.azure.com/".into(),
            "--api-version".into(),
            "2024-12-01-preview".into(),
            "--api-key-env".into(),
            "__AZURE_TEST_KEY__".into(),
        ])
        .expect("apply ok");

        assert_eq!(v["ok"].as_bool(), Some(true));
        assert_eq!(v["provider"].as_str(), Some("azure"));
        assert_eq!(
            v["base_url"].as_str(),
            Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview")
        );

        let text = std::fs::read_to_string(&cfg_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["agent"]["provider"].as_str(), Some("azure"));
        assert_eq!(parsed["agent"]["model"].as_str(), Some("my-deployment"));
        assert_eq!(
            parsed["agent"]["base_url"].as_str(),
            Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview")
        );

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn azure_apply_rejects_deployment_in_base_url() {
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!(
            "cos-setup-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        let err = run(&[
            "llm".into(),
            "apply".into(),
            "--provider".into(),
            "azure".into(),
            "--model".into(),
            "my-deployment".into(),
            "--base-url".into(),
            "https://acme.openai.azure.com/openai/deployments/my-deployment".into(),
            "--api-version".into(),
            "2024-12-01-preview".into(),
            "--api-key-env".into(),
            "__AZURE_TEST_KEY__".into(),
        ])
        .unwrap_err();
        assert!(err.contains("resource root"), "msg was: {err}");

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn status_media_reports_base_url_and_api_version_split() {
        // status_media reads fresh from disk per call (unlike
        // status_llm which uses the OnceLock-cached config), so we can
        // exercise the new endpoint/api_version fields here without
        // racing the global config. Azure isn't a media provider in
        // the spec catalogue, so simulate the same persisted shape on
        // a TTS block (the parser is provider-agnostic).
        let _g = env_lock();
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);
        std::fs::write(
            &cfg_path,
            r#"{"tts":{"provider":"openai","model":"tts-1","base_url":"https://acme.example.com/v1?api-version=2024-12-01-preview","api_key_env":"__TTS_TEST_KEY__"}}"#,
        )
        .unwrap();

        let v = status_cmd(Modality::Tts).expect("status ok");
        assert_eq!(v["provider"].as_str(), Some("openai"));
        assert_eq!(
            v["base_url"].as_str(),
            Some("https://acme.example.com/v1?api-version=2024-12-01-preview")
        );
        assert_eq!(
            v["endpoint"].as_str(),
            Some("https://acme.example.com/v1")
        );
        assert_eq!(v["api_version"].as_str(), Some("2024-12-01-preview"));

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}

//! `cos agent setup` — per-modality config wizard.
//!
//! Replaces the previous `cos agent onboarding` family. Pick a
//! modality (llm / tts / stt / imagegen / embed / all), then walk
//! through provider → model → API key → persist → optional probe.
//! Each modality writes to its own `~/.config/cos/config.json` block
//! (`[agent]`, `[tts]`, `[stt]`, `[imagegen]`, `[embed]`) and stores
//! credentials in the `agent` namespace of the per-user credential
//! store (`~/.local/share/cos/credentials/agent/`).
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
use std::time::{Duration, Instant};

use crate::agent::llm;

pub fn run(args: &[String]) -> Result<Value, String> {
    // Parse: optional --no-verify / --verify-only / --status / --reset /
    // --providers / --help, plus a required leading positional modality:
    //   llm | tts | stt | imagegen | embed | all
    //
    // Extra non-interactive subcommands take per-modality flags:
    //   apply       --provider X --model Y [--api-key K | --api-key-stdin | --api-key-env E]
    //   test        (alias for --verify-only)
    //   oauth-start --provider copilot
    //   oauth-poll  --provider copilot --device-code <code>
    //   models      --provider copilot
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
    let mut oauth_device_code: Option<String> = None;

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
            "oauth-start" if sub.is_none() => sub = Some("oauth-start"),
            "oauth-poll" if sub.is_none() => sub = Some("oauth-poll"),
            "models" if sub.is_none() => sub = Some("models"),
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
            "--device-code" => {
                i += 1;
                if i >= args.len() {
                    return Err("--device-code requires a value".into());
                }
                oauth_device_code = Some(args[i].clone());
            }
            other => {
                if let Some(m) = Modality::parse(other) {
                    if let Some(existing) = modality {
                        return Err(format!(
                            "specify a modality only once (got both `{}` and `{other}`)",
                            existing.name()
                        ));
                    }
                    modality = Some(m);
                } else if other.starts_with('-') {
                    return Err(format!(
                        "unknown setup flag: {other}. try: --no-verify | --verify-only | --status | --reset | --providers | --device-code"
                    ));
                } else if sub.is_none() {
                    return Err(format!(
                        "unknown setup modality/subcommand: {other}. try: llm | tts | stt | imagegen | embed | all | apply | test | oauth-start | oauth-poll | models | --status | --reset | --providers | --verify-only"
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
    let oauth_subcommand = matches!(sub, Some("oauth-start") | Some("oauth-poll") | Some("models"));
    if apply_flags_set && sub != Some("apply") && !oauth_subcommand {
        return Err(
            "--provider / --model / --api-key{,-stdin,-env} / --base-url / --api-version are only valid with the `apply` / `oauth-*` subcommands"
                .into(),
        );
    }
    if oauth_device_code.is_some() && sub != Some("oauth-poll") {
        return Err("--device-code is only valid with the `oauth-poll` subcommand".into());
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
            Some("oauth-start") => oauth_start_cmd(apply_provider.as_deref()),
            Some("oauth-poll") => {
                oauth_poll_cmd(apply_provider.as_deref(), oauth_device_code.as_deref())
            }
            Some("models") => models_cmd(apply_provider.as_deref()),
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
        provider: apply_provider.clone(),
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
        Some("oauth-start") => oauth_start_cmd(apply_provider.as_deref()),
        Some("oauth-poll") => {
            oauth_poll_cmd(apply_provider.as_deref(), oauth_device_code.as_deref())
        }
        Some("models") => models_cmd(apply_provider.as_deref()),
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
            "stt":       "Speech-to-text (under [stt]). Used by voice input / `cos model transcribe`.",
            "imagegen":  "Image generation (under [imagegen]). Used by `cos agent image`.",
            "embed":     "Text embeddings (under [embed]). Used by semantic memory / `cos agent recall`.",
            "all":       "Walk every modality in order, prompting before each.",
        },
        "subcommands": {
            "apply":       "Non-interactive write. Required flags: --provider X --model Y. Credential: one of --api-key K | --api-key-stdin | --api-key-env ENV. `--provider none` clears the modality.",
            "test":        "Alias for --verify-only: probe the currently configured provider for this modality.",
            "providers":   "Emit JSON catalogue of providers + sample models for the picked modality (or `all`). Used by the cosmic-settings agent page.",
            "oauth-start": "Begin a device-authorization flow for a provider whose `auth_kind` is `oauth_device` (currently only `copilot`). Requires --provider X. Emits the user code + verification URL the UI should display, plus the device_code the UI passes to `oauth-poll`.",
            "oauth-poll":  "One-shot poll for an in-flight OAuth flow. Requires --provider X --device-code Z. Emits a `status` of pending | slow_down | ok | expired | denied | error. On `ok` the long-lived credential is stored automatically; the UI then refreshes its model list via `models`.",
            "models":      "Fetch usable chat models for providers whose `auth_kind` is `oauth_device` (currently only `copilot`). Requires --provider X. Returns `{ models: [{name, wire_api}, …] }`; non-chat and unsupported endpoint models are excluded.",
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
            "--device-code C":  "(oauth-poll only) The opaque device_code returned by `oauth-start`. The UI keeps it private and only forwards it to the kernel.",
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
            "cos agent setup llm oauth-start --provider copilot                   # GitHub Copilot device-flow start",
            "cos agent setup llm oauth-poll  --provider copilot --device-code D   # poll until status=ok",
            "cos agent setup llm apply       --provider copilot --model gpt-4o    # reuse stored token (no --api-key)",
            "cos agent setup llm models      --provider copilot                   # refresh Copilot model list",
        ],
        "notes": [
            "Bare `cos agent setup` (no args) opens an interactive picker on a TTY; on a non-TTY it errors and lists the modalities.",
            "All subcommands emit JSON on success; errors are plain strings on stderr.",
        ],
    })
}

/// Probe the currently configured provider without re-running the
/// wizard. Useful after editing `~/.config/cos/config.json` by hand
/// or rotating an API key.
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
    if cfg.provider.is_empty() {
        return Err(json!({
            "error": "agent not configured",
            "fix": "cos agent setup llm",
            "details": "no LLM provider is configured. Run `cos agent setup llm` to pick one (or use the desktop initial-setup AI page).",
        })
        .to_string());
    }
    if cfg.provider == "mock" {
        return Err(json!({
            "error": "agent not configured",
            "fix": "cos agent setup llm",
            "details": "the `mock` provider returns canned answers. Run `cos agent setup llm` to pick a real LLM provider.",
        })
        .to_string());
    }
    if !provider_needs_credential(&cfg.provider) {
        return Ok(());
    }
    if credential_present(cfg) {
        return Ok(());
    }
    if llm::registry::build(&cfg.provider, &cfg.model, cfg)
        .is_ok_and(|provider| provider.is_configured())
    {
        return Ok(());
    }
    if cfg.provider_fallbacks.iter().any(|fallback| {
        let fallback_cfg = fallback.apply_to(cfg);
        !fallback_cfg.provider.is_empty()
            && fallback_cfg.provider != "mock"
            && (!provider_needs_credential(&fallback_cfg.provider)
                || credential_present(&fallback_cfg)
                || llm::registry::build(
                    &fallback_cfg.provider,
                    &fallback_cfg.model,
                    &fallback_cfg,
                )
                .is_ok_and(|provider| provider.is_configured()))
    }) {
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
        "provider_fallbacks": fallback_status(cfg),
        "api_key_credential": cfg.api_key_credential,
        "api_key_env": cfg.api_key_env,
        "base_url": cfg.base_url,
        "endpoint": base_url,
        "api_version": api_version,
        "config_path": config_path().display().to_string(),
        "reason": reason,
    })
}

fn fallback_status(cfg: &crate::config::AgentConfig) -> Vec<Value> {
    cfg.provider_fallbacks
        .iter()
        .map(|fallback| {
            let mut header_names = fallback
                .extra_headers
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            header_names.sort();
            json!({
                "provider": fallback.provider,
                "model": fallback.model,
                "api_key_credential": fallback.api_key_credential,
                "api_key_env": fallback.api_key_env,
                "api_key_credentials": fallback.api_key_credentials,
                "api_key_envs": fallback.api_key_envs,
                "base_url": fallback.base_url,
                "extra_header_names": header_names,
                "request_timeout": fallback.request_timeout,
                "pool_strategy": fallback.pool_strategy,
                "pool_cooldown_secs": fallback.pool_cooldown_secs,
                "aws_region": fallback.aws_region,
                "aws_access_key_credential": fallback.aws_access_key_credential,
                "aws_access_key_env": fallback.aws_access_key_env,
                "aws_secret_key_credential": fallback.aws_secret_key_credential,
                "aws_secret_key_env": fallback.aws_secret_key_env,
                "aws_session_token_credential": fallback.aws_session_token_credential,
                "aws_session_token_env": fallback.aws_session_token_env,
            })
        })
        .collect()
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
            "hint": "run it in a terminal, or write ~/.config/cos/config.json and the `agent` credential manually",
        })
        .to_string());
    }

    let stderr = std::io::stderr();
    let mut e = stderr.lock();
    let _ = writeln!(e, "cos agent setup llm — conversational LLM wizard");
    let _ = writeln!(e);

    // ---- Step 1: provider ------------------------------------------------
    let providers = user_facing_providers();
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
    let mut credential_name: Option<String> = None;
    let mut credential_env: Option<String> = None;
    let mut oauth_live_models: Option<Vec<String>> = None;

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

    // ---- Step 1c: OAuth-device sign-in ----------------------------------
    //
    // Terminal / headless environments (WSL, Docker, SSH) cannot rely on a
    // browser handoff. Drive the device flow entirely in the TTY: print the
    // GitHub URL + user code, then poll until GitHub reports success/failure.
    if auth_kind_for(&provider) == Some("oauth_device") {
        let login = oauth_device_terminal_login(&provider, &mut e)?;
        credential_name = Some(login.credential_name);
        oauth_live_models = Some(login.models);
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
    let model = if let Some(models) = oauth_live_models.as_deref() {
        pick_oauth_model(&provider, models, &mut e)?
    } else if !known.is_empty() {
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
    if auth_kind_for(&provider) == Some("oauth_device") {
        let _ = writeln!(
            e,
            "(GitHub sign-in stored as `{}`; skipping API-key paste)",
            credential_name.as_deref().unwrap_or(COPILOT_GITHUB_TOKEN_CREDENTIAL)
        );
    } else if provider_needs_credential(&provider) {
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

const LOCAL_EMBED_PROVIDER: &str = "local";
const LOCAL_EMBED_MODEL: &str = "qwen3-embedding-0.6b";

fn is_embed_spec(spec: &media::ModalitySpec) -> bool {
    spec.name == "embed"
}

fn is_local_embed_provider(provider: &str) -> bool {
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

fn local_embed_precheck(snap: &Value, model: Option<&str>) -> Result<(), String> {
    let cfg = local_embed_config_from_snapshot(snap, model);
    crate::model::tasks::qwen3_genai::precheck(&cfg)
}

fn local_embed_unavailable_details(err: &str) -> String {
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

fn default_provider_for(spec: &media::ModalitySpec) -> &'static str {
    if is_embed_spec(spec) && local_embed_precheck(&json!({}), None).is_err() {
        "openai"
    } else {
        spec.default_provider
    }
}

/// Run the wizard for one media modality (TTS / STT / ImageGen / Embed).
fn wizard_media(spec: &'static media::ModalitySpec, verify_after: bool) -> Result<Value, String> {
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
        let marker = if p.name == default_provider { " (default)" } else { "" };
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
            "hint": "set up each modality non-interactively by editing ~/.config/cos/config.json",
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
    let raw_provider = snap
        .get("provider")
        .and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let embed_auto = matches!(modality, Modality::Embed)
        && raw_provider
            .as_deref()
            .map(|p| p == "auto")
            .unwrap_or(true);
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
    let mut model = snap.get("model").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if model.is_empty() {
        if matches!(modality, Modality::Embed) && is_local_embed_provider(&provider) {
            model = LOCAL_EMBED_MODEL.to_string();
        } else if let Some(choice) = spec.providers.iter().find(|p| p.name == provider) {
            model = choice.default_model.to_string();
        }
    }
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
    // `reason` is emitted as a structured envelope (`error` /
    // `details` / `fix`) matching the shape the cosmic-settings UI
    // expects in [`ErrorEnvelope`]. `status_llm` (line 568) emits the
    // same shape via `is_ready`; emitting a plain string here used to
    // make every media settings page fail with "invalid status JSON:
    // invalid type: string, expected struct ErrorEnvelope".
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

/// Providers shown in user-facing pickers (`cos agent setup llm` wizard,
/// the desktop catalogue consumed by cosmic-settings, the initial-setup
/// AI page). Filters out `mock` (test-only) and `llama_local` (managed
/// via `cos model load`, not the standard credential flow). Power users
/// can still address these by name via `cos agent setup llm apply
/// --provider mock ...` or by editing ~/.config/cos/config.json directly —
/// only the lists are hidden, not the registry.
fn user_facing_providers() -> Vec<&'static str> {
    llm::available_providers()
        .into_iter()
        .filter(|name| !matches!(*name, "mock" | "llama_local"))
        .collect()
}

fn providers_llm() -> Value {
    let names = user_facing_providers();
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
            let default_model = default_model_name(name).unwrap_or_else(|| {
                models.first().map(|m| m.name.to_string()).unwrap_or_default()
            });
            let mut entry = json!({
                "name": name,
                "label": *name,
                "needs_credential": provider_needs_credential(name),
                "default_env": default_env_name(name),
                "models": model_list,
                "default_model": default_model,
                "extra_fields": extra_fields_for(name),
            });
            if let Some(kind) = auth_kind_for(name) {
                entry
                    .as_object_mut()
                    .expect("entry is object")
                    .insert("auth_kind".into(), json!(kind));
            }
            entry
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
    let default_provider = default_provider_for(spec);
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
        "default_provider": default_provider,
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

/// Authentication kind exposed to UIs that can render something other
/// than a paste-an-API-key form. Currently the only non-default value
/// is `"oauth_device"` for GitHub Copilot, which expects the UI to
/// drive the device-authorization dance via the `oauth-start` and
/// `oauth-poll` subcommands. Returning `None` means the standard
/// API-key form is correct.
fn auth_kind_for(provider: &str) -> Option<&'static str> {
    match provider {
        "copilot" => Some("oauth_device"),
        _ => None,
    }
}

/// Default model the picker should pre-select for providers whose
/// `llm::metadata` catalogue is intentionally empty (because the
/// real model list is fetched live post-sign-in). Returns `None`
/// for providers backed by the static catalogue — those keep using
/// the first metadata entry as their default.
fn default_model_name(provider: &str) -> Option<String> {
    match provider {
        "copilot" => Some("gpt-4o".into()),
        _ => None,
    }
}

/// Credential name the OAuth-device path stores the long-lived GitHub
/// token under in the `agent` namespace. Centralised so the kernel
/// readers (apply, providers catalog, openai_compat) and the writer
/// (oauth-poll) agree on a single string.
const COPILOT_GITHUB_TOKEN_CREDENTIAL: &str = "copilot_github_token";
const MIN_OAUTH_POLL_SECS: u64 = 5;

struct OAuthTerminalLogin {
    credential_name: String,
    models: Vec<String>,
}

// ---------------------------------------------------------------------------
// OAuth + model-discovery subcommands (Copilot)
// ---------------------------------------------------------------------------

fn oauth_device_terminal_login(
    provider: &str,
    e: &mut impl Write,
) -> Result<OAuthTerminalLogin, String> {
    ensure_oauth_provider(provider)?;
    match provider {
        "copilot" => copilot_terminal_login(e),
        other => Err(format!("oauth terminal login: unsupported provider `{other}`")),
    }
}

fn copilot_terminal_login(e: &mut impl Write) -> Result<OAuthTerminalLogin, String> {
    let _ = writeln!(e);
    let _ = writeln!(e, "GitHub Copilot sign-in");
    let _ = writeln!(
        e,
        "This terminal flow works in WSL, Docker, SSH, and other headless Linux environments."
    );

    if let Some(github_token) = crate::credential::try_load(COPILOT_GITHUB_TOKEN_CREDENTIAL, "agent")
        .map_err(|err| format!("read credential `{COPILOT_GITHUB_TOKEN_CREDENTIAL}`: {err}"))?
        .filter(|token| !token.trim().is_empty())
    {
        match fetch_copilot_model_names(&github_token) {
            Ok(models) => {
                let _ = writeln!(
                    e,
                    "Existing GitHub sign-in found in `{}`; reusing it.",
                    COPILOT_GITHUB_TOKEN_CREDENTIAL
                );
                return Ok(OAuthTerminalLogin {
                    credential_name: COPILOT_GITHUB_TOKEN_CREDENTIAL.to_string(),
                    models,
                });
            }
            Err(err) => {
                let _ = writeln!(
                    e,
                    "Stored GitHub sign-in could not be used ({err}); starting a new device login."
                );
            }
        }
    }

    let dc = block_on(llm::providers::copilot_auth::start_device_flow())?
        .map_err(|err| format!("oauth-start: {err}"))?;
    let mut interval = dc.interval.max(MIN_OAUTH_POLL_SECS);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(dc.expires_in))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(dc.expires_in.min(900)));

    let _ = writeln!(e);
    let _ = writeln!(e, "Open this URL in any browser:");
    let _ = writeln!(e, "  {}", dc.verification_uri);
    let _ = writeln!(e, "Enter this code:");
    let _ = writeln!(e, "  {}", dc.user_code);
    let _ = writeln!(e);
    let _ = writeln!(
        e,
        "Waiting for GitHub approval (expires in {}s)...",
        dc.expires_in
    );
    let _ = e.flush();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "GitHub device code expired before approval; rerun setup to try again".into(),
            );
        }
        std::thread::sleep(Duration::from_secs(interval).min(remaining));

        let outcome = block_on(llm::providers::copilot_auth::poll_device_flow(&dc.device_code))?
            .map_err(|err| format!("oauth-poll: {err}"))?;
        use llm::providers::copilot_auth::PollOutcome;
        match outcome {
            PollOutcome::Pending => {
                let _ = write!(e, ".");
                let _ = e.flush();
            }
            PollOutcome::SlowDown { interval: next } => {
                interval = next.max(MIN_OAUTH_POLL_SECS);
                let _ = writeln!(e);
                let _ = writeln!(e, "GitHub asked us to slow down; polling every {interval}s.");
                let _ = e.flush();
            }
            PollOutcome::Expired => {
                let _ = writeln!(e);
                return Err(
                    "GitHub device code expired before approval; rerun setup to try again".into(),
                );
            }
            PollOutcome::Denied => {
                let _ = writeln!(e);
                return Err("GitHub Copilot sign-in was denied".into());
            }
            PollOutcome::Authorized { github_token, .. } => {
                let _ = writeln!(e);
                store_credential(COPILOT_GITHUB_TOKEN_CREDENTIAL, &github_token).map_err(|err| {
                    format!(
                        "oauth-poll: stored token rejected by credential store: {err}\n\
                         hint: rerun as a user with write access to the agent credential namespace."
                    )
                })?;
                let _ = writeln!(
                    e,
                    "✓ GitHub sign-in complete; credential stored as `{}`",
                    COPILOT_GITHUB_TOKEN_CREDENTIAL
                );
                let models = match fetch_copilot_model_names(&github_token) {
                    Ok(models) => models,
                    Err(err) => {
                        let _ = writeln!(
                            e,
                            "⚠  signed in, but Copilot model discovery failed: {err}"
                        );
                        Vec::new()
                    }
                };
                return Ok(OAuthTerminalLogin {
                    credential_name: COPILOT_GITHUB_TOKEN_CREDENTIAL.to_string(),
                    models,
                });
            }
        }
    }
}

fn fetch_copilot_model_names(github_token: &str) -> Result<Vec<String>, String> {
    Ok(model_names_from_values(block_on(fetch_copilot_models(
        github_token,
    ))??))
}

fn model_names_from_values(values: Vec<Value>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|v| {
            v.get("name")
                .and_then(|name| name.as_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn pick_oauth_model(
    provider: &str,
    models: &[String],
    e: &mut impl Write,
) -> Result<String, String> {
    let preferred_default = default_model_name(provider);
    let default = if models.is_empty() {
        preferred_default.unwrap_or_default()
    } else {
        preferred_default
            .filter(|candidate| models.iter().any(|m| m == candidate))
            .or_else(|| models.first().cloned())
            .unwrap_or_default()
    };

    if models.is_empty() {
        let _ = writeln!(
            e,
            "No live model list was returned for `{provider}` — enter a Copilot model identifier."
        );
    } else {
        let _ = writeln!(e, "Available models for `{provider}`:");
        for (i, model) in models.iter().enumerate() {
            let _ = writeln!(e, "  {:>2}. {}", i + 1, model);
        }
    }

    if models.is_empty() {
        if default.is_empty() {
            let _ = write!(e, "Model name: ");
        } else {
            let _ = write!(e, "Model name [{default}]: ");
        }
    } else if default.is_empty() {
        let _ = write!(e, "Pick a number (1-{}) or type a model name: ", models.len());
    } else {
        let _ = write!(
            e,
            "Pick a number (1-{}) or type a model name [{}]: ",
            models.len(),
            default
        );
    }
    let _ = e.flush();
    let raw = read_line()?.trim().to_string();
    if raw.is_empty() {
        if default.is_empty() {
            return Err("model name cannot be empty".into());
        }
        return Ok(default);
    }

    if !models.is_empty() {
        if let Ok(idx) = raw.parse::<usize>() {
            if idx < 1 || idx > models.len() {
                return Err(format!(
                    "out of range: {idx} (expected 1-{})",
                    models.len()
                ));
            }
            return Ok(models[idx - 1].clone());
        }
        if !models.iter().any(|m| m == &raw) {
            let _ = writeln!(e);
            let _ = writeln!(e, "⚠  `{raw}` was not returned by Copilot model discovery.");
            let _ = write!(e, "Use it anyway? (y/N): ");
            let _ = e.flush();
            let yn = read_line()?.trim().to_ascii_lowercase();
            if !matches!(yn.as_str(), "y" | "yes") {
                return Err("aborted: unknown Copilot model name".into());
            }
        }
    }
    Ok(raw)
}

fn require_provider(provider: Option<&str>, sub: &str) -> Result<String, String> {
    match provider {
        Some(p) if !p.trim().is_empty() => Ok(p.trim().to_string()),
        _ => Err(format!(
            "`{sub}` requires --provider <name>. Only `copilot` is supported today."
        )),
    }
}

fn ensure_oauth_provider(provider: &str) -> Result<(), String> {
    match auth_kind_for(provider) {
        Some(_) => Ok(()),
        None => Err(format!(
            "provider `{provider}` does not use OAuth device flow. \
             Use `apply --api-key{{,-stdin,-env}}` instead."
        )),
    }
}

fn block_on<F, T>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    Ok(rt.block_on(fut))
}

/// `cos agent setup oauth-start --provider copilot` → emits the
/// device-authorization codes the UI shows the user.
fn oauth_start_cmd(provider: Option<&str>) -> Result<Value, String> {
    let provider = require_provider(provider, "oauth-start")?;
    ensure_oauth_provider(&provider)?;
    // Only Copilot for now — keep the dispatch explicit so adding a
    // second OAuth provider is a visible diff.
    if provider != "copilot" {
        return Err(format!("oauth-start: unsupported provider `{provider}`"));
    }
    let dc = block_on(llm::providers::copilot_auth::start_device_flow())?
        .map_err(|e| format!("oauth-start: {e}"))?;
    Ok(json!({
        "provider": provider,
        "device_code": dc.device_code,
        "user_code": dc.user_code,
        "verification_uri": dc.verification_uri,
        "expires_in": dc.expires_in,
        "interval": dc.interval,
    }))
}

/// `cos agent setup oauth-poll --provider copilot --device-code <code>`
/// → single poll. The UI loops on its own schedule and stops when this
/// command emits `status: "ok"` (token stored) or a terminal failure.
fn oauth_poll_cmd(provider: Option<&str>, device_code: Option<&str>) -> Result<Value, String> {
    let provider = require_provider(provider, "oauth-poll")?;
    ensure_oauth_provider(&provider)?;
    if provider != "copilot" {
        return Err(format!("oauth-poll: unsupported provider `{provider}`"));
    }
    let code = device_code
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "oauth-poll requires --device-code <code>".to_string())?;
    let outcome = block_on(llm::providers::copilot_auth::poll_device_flow(code))?
        .map_err(|e| format!("oauth-poll: {e}"))?;
    use llm::providers::copilot_auth::PollOutcome;
    match outcome {
        PollOutcome::Pending => Ok(json!({"status": "pending"})),
        PollOutcome::SlowDown { interval } => {
            Ok(json!({"status": "slow_down", "interval": interval}))
        }
        PollOutcome::Expired => Ok(json!({"status": "expired"})),
        PollOutcome::Denied => Ok(json!({"status": "denied"})),
        PollOutcome::Authorized { github_token, .. } => {
            // Persist the long-lived GitHub token so subsequent `apply`
            // + chat traffic can exchange it for short-lived Copilot
            // tokens on demand. We store under a fixed credential name
            // so a re-sign-in cleanly overwrites the prior token.
            store_credential(COPILOT_GITHUB_TOKEN_CREDENTIAL, &github_token).map_err(|e| {
                format!(
                    "oauth-poll: stored token rejected by credential store: {e}\n\
                     hint: rerun as a user with write access to the agent credential namespace."
                )
            })?;
            Ok(json!({
                "status": "ok",
                "provider": provider,
                "credential": COPILOT_GITHUB_TOKEN_CREDENTIAL,
            }))
        }
    }
}

/// `cos agent setup models --provider copilot` → fetch the live model
/// catalogue from Copilot's `/models` endpoint using the stored token.
/// Returns selectable chat models with their negotiated wire protocol.
/// Embedding, internal, disabled, and unsupported-endpoint entries are
/// excluded before UIs render the dropdown.
fn models_cmd(provider: Option<&str>) -> Result<Value, String> {
    let provider = require_provider(provider, "models")?;
    if provider != "copilot" {
        return Err("models: live discovery is only supported for `copilot` today; \
             other providers expose their model lists via `--providers`".to_string());
    }
    let github_token = crate::credential::try_load(COPILOT_GITHUB_TOKEN_CREDENTIAL, "agent")
        .map_err(|e| format!("read credential `{COPILOT_GITHUB_TOKEN_CREDENTIAL}`: {e}"))?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            "GitHub Copilot is not signed in. Run `oauth-start` + `oauth-poll` first \
             (or use the desktop AI settings page)."
                .to_string()
        })?;

    let models = block_on(fetch_copilot_models(&github_token))??;
    Ok(json!({
        "provider": provider,
        "models": models,
    }))
}

async fn fetch_copilot_models(
    github_token: &str,
) -> Result<Vec<Value>, String> {
    let copilot = llm::providers::copilot_auth::ensure_copilot_token(github_token)
        .await
        .map_err(|e| format!("copilot auth: {e}"))?;
    let models = llm::providers::copilot_auth::ensure_copilot_models(&copilot)
        .await
        .map_err(|e| format!("Copilot /models: {e}"))?;

    let mut out: Vec<Value> = models
        .iter()
        .filter(|model| model.is_selectable_chat_model())
        .filter_map(|model| {
            let wire_api = model.wire_api()?;
            Some(json!({
                "name": model.id,
                "wire_api": wire_api.config_name(),
            }))
        })
        .collect();
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    Ok(out)
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // `&s[..max]` would panic if `max` lands inside a multi-byte
        // UTF-8 codepoint (provider error bodies routinely include
        // non-ASCII text). Walk back to the nearest char boundary.
        format!("{}…", crate::agent::util::char_safe_truncate(s, max))
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
    let (credential_name, credential_env) = if auth_kind_for(&provider) == Some("oauth_device") {
        resolve_oauth_credential(&args, &provider)?
    } else {
        resolve_key_args(&args, &provider, &credential_hint, needs_cred)?
    };

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
    if is_embed_spec(spec) && is_local_embed_provider(provider.name) {
        if let Err(err) = local_embed_precheck(&json!({}), Some(&model)) {
            return Err(json!({
                "error": "local embedding stack unavailable",
                "details": local_embed_unavailable_details(&err),
                "fix": "cos agent setup embed",
            })
            .to_string());
        }
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

/// Resolver for OAuth-device providers (currently only `copilot`).
/// The credential is established out-of-band by `oauth-poll`, so
/// `apply` either reuses the stored token name or accepts an explicit
/// env override for users who want to inject a GitHub token from CI.
fn resolve_oauth_credential(
    args: &ApplyArgs,
    provider_name: &str,
) -> Result<(Option<String>, Option<String>), String> {
    if args.api_key.is_some() || args.api_key_stdin {
        return Err(format!(
            "provider `{provider_name}` uses OAuth device flow; \
             use `cos agent setup llm oauth-start` instead of --api-key/--api-key-stdin. \
             (--api-key-env is still accepted for non-interactive overrides.)"
        ));
    }
    if let Some(env) = args.api_key_env.as_deref() {
        let env = env.trim();
        if env.is_empty() {
            return Err("--api-key-env cannot be empty".into());
        }
        return Ok((None, Some(env.to_string())));
    }
    let credential_name = match provider_name {
        "copilot" => COPILOT_GITHUB_TOKEN_CREDENTIAL,
        other => {
            return Err(format!(
                "internal: provider `{other}` advertises auth_kind=oauth_device but no credential mapping is defined"
            ));
        }
    };
    let exists = crate::credential::try_load(credential_name, "agent")
        .map_err(|e| format!("read credential `{credential_name}`: {e}"))?
        .filter(|s| !s.trim().is_empty())
        .is_some();
    if !exists {
        return Err(format!(
            "provider `{provider_name}` is not signed in yet. \
             Run `cos agent setup llm oauth-start --provider {provider_name}` first \
             (or use the desktop AI settings page), or pass --api-key-env <ENV> to use \
             a GitHub token from the environment."
        ));
    }
    Ok((Some(credential_name.to_string()), None))
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
    std::env::var_os("COS_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(crate::paths::user_config_path)
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
    let json_text =
        serde_json::to_string_pretty(cfg).map_err(|e| format!("serialize config: {e}"))?;
    // Crash-safe: shared helper writes a per-process tmp file, fsyncs
    // both the tmp and the parent dir, then renames into place. This
    // replaces an earlier `fs::write(<single>.tmp) + fs::rename` which
    // (a) skipped fsync entirely so a power loss between write and
    // rename could surface a torn or empty file at recovery time, and
    // (b) used a shared tmp filename so two concurrent setup wizards
    // for different modalities could clobber each other's tmp data.
    crate::agent::util::atomic_write_with_fsync(path, json_text.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    // Best-effort: tighten permissions on POSIX since the file may
    // carry API keys when the wizard isn't using a credential store.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
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
mod tests;

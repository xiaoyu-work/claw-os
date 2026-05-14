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
    // Parse: optional --no-verify / --verify-only / --status / --reset / --help.
    // The wizard is the default (no positional/subcommand).
    let mut verify_after = true;
    let mut explicit_verify = false;
    let mut sub: Option<&str> = None;

    for a in args {
        match a.as_str() {
            "--no-verify" => verify_after = false,
            "--verify" => verify_after = true,
            "--verify-only" => explicit_verify = true,
            "--status" | "status" if sub.is_none() => sub = Some("status"),
            "--reset" | "reset" if sub.is_none() => sub = Some("reset"),
            "-h" | "--help" if sub.is_none() => sub = Some("help"),
            other => {
                if sub.is_none() && !other.starts_with('-') {
                    return Err(format!(
                        "unknown setup subcommand: {other}. try: (no args for wizard) | --status | --reset | --verify-only | --no-verify"
                    ));
                } else if other.starts_with('-') {
                    return Err(format!(
                        "unknown setup flag: {other}. try: --no-verify | --verify-only | --status | --reset"
                    ));
                }
            }
        }
    }

    if explicit_verify {
        return verify_cmd();
    }
    match sub {
        Some("status") => status_cmd(),
        Some("reset") => reset_cmd(),
        Some("help") => Ok(help_doc()),
        _ => wizard_cmd(verify_after),
    }
}

fn help_doc() -> Value {
    json!({
        "command": "cos agent setup",
        "summary": "First-run wizard: pick an LLM provider, a model, store an API key, and verify it works.",
        "subcommands": {
            "(no args)":     "Run the interactive wizard (requires TTY). After saving config, probes the provider to confirm the key actually works.",
            "--no-verify":   "Skip the live provider probe at the end of the wizard.",
            "--verify-only": "Skip the wizard; just probe the currently configured provider.",
            "--status":      "Show whether the agent is configured to talk to a real provider.",
            "--reset":       "Revert the agent block of the config to the built-in mock defaults.",
        },
    })
}

/// Probe the currently configured provider without re-running the
/// wizard. Useful after editing `/etc/cos/config.json` by hand or
/// rotating an API key.
fn verify_cmd() -> Result<Value, String> {
    let cfg = &crate::config::get().agent;
    if let Err(reason) = is_ready(cfg) {
        return Ok(json!({
            "ok": false,
            "attempted": false,
            "reason": reason,
            "hint": "run `cos agent setup` to configure a provider first",
        }));
    }
    if !provider_needs_credential(&cfg.provider) {
        return Ok(json!({
            "ok": true,
            "attempted": false,
            "reason": format!("provider `{}` does not need credentials; skipping probe", cfg.provider),
            "provider": cfg.provider,
            "model": cfg.model,
        }));
    }
    let mut e = std::io::stderr();
    let _ = writeln!(e, "probing {} ({}) — up to 30s...", cfg.provider, cfg.model);
    let verdict = super::run_active_provider_probe(&cfg.provider, cfg, 30);
    let ok = verdict.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        let _ = writeln!(e, "✓ probe succeeded");
    } else {
        let _ = writeln!(e, "✗ probe failed");
        if let Some(msg) = verdict.get("error_message").and_then(|v| v.as_str()) {
            let _ = writeln!(e, "  {msg}");
        }
        let _ = writeln!(e, "  hint: re-run `cos agent setup` or fix the credential, then `cos agent setup --verify-only`");
    }
    Ok(json!({
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

fn status_cmd() -> Result<Value, String> {
    let cfg = &crate::config::get().agent;
    let ready = is_ready(cfg);
    Ok(json!({
        "ready": ready.is_ok(),
        "provider": cfg.provider,
        "model": cfg.model,
        "api_key_credential": cfg.api_key_credential,
        "api_key_env": cfg.api_key_env,
        "config_path": config_path().display().to_string(),
        "reason": ready.err(),
    }))
}

fn reset_cmd() -> Result<Value, String> {
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

fn wizard_cmd(verify_after: bool) -> Result<Value, String> {
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
    let known = llm::metadata::list_for_provider(&provider);
    let _ = writeln!(e);
    if !known.is_empty() {
        let _ = writeln!(e, "Known models for `{provider}`:");
        for m in &known {
            let _ = writeln!(e, "  - {}", m.name);
        }
    } else {
        let _ = writeln!(
            e,
            "No bundled model metadata for `{provider}` — enter a model identifier."
        );
    }
    let _ = write!(e, "Model name: ");
    let _ = e.flush();
    let model = read_line()?.trim().to_string();
    if model.is_empty() {
        return Err("model name cannot be empty".into());
    }

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
        assert_eq!(v.get("command").and_then(|s| s.as_str()), Some("cos agent setup"));
        assert!(v.get("subcommands").and_then(|s| s.as_object()).is_some());
    }

    #[test]
    fn reset_writes_mock_provider_to_config() {
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

        let v = reset_cmd().expect("reset ok");
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
    fn status_returns_provider_and_ready_flag() {
        // Use a tmp config path so we don't depend on /etc/cos/config.json.
        let tmp_dir = std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let cfg_path = tmp_dir.join("config.json");
        std::env::set_var("COS_CONFIG_PATH", &cfg_path);

        let v = status_cmd().expect("status ok");
        assert!(v.get("ready").and_then(|b| b.as_bool()).is_some());
        assert!(v.get("provider").and_then(|s| s.as_str()).is_some());

        std::env::remove_var("COS_CONFIG_PATH");
        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}

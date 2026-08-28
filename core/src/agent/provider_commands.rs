use super::{llm, setup};
use serde_json::{json, Value};

pub(super) fn status_cmd() -> Result<Value, String> {
    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
    let daemon = crate::clawd::agent_client::daemon_status()?;
    let ready = setup::is_ready(cfg);
    let key_source = match setup::resolved_key_source(cfg) {
        Ok(Some(source)) => source.to_json(),
        Ok(None) | Err(_) => Value::Null,
    };

    let last_session = match crate::agent::memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => match db.sessions(1) {
            Ok(mut sessions) if !sessions.is_empty() => {
                let session = sessions.remove(0);
                json!({
                    "session_id": session.session_id,
                    "title": session.title,
                    "last_ts_ms": session.last_ts_ms,
                    "message_count": session.message_count,
                })
            }
            _ => Value::Null,
        },
        Err(_) => Value::Null,
    };

    let (ready_ok, ready_reason, fix, readiness_error) = match ready {
        Ok(()) => (true, Value::Null, Value::Null, Value::Null),
        Err(reason_json) => {
            let parsed: Value =
                serde_json::from_str(&reason_json).unwrap_or_else(|_| json!(reason_json));
            let error = parsed
                .get("error")
                .and_then(|value| value.as_str())
                .map(|value| json!(value))
                .unwrap_or(parsed.clone());
            let fix = parsed
                .get("fix")
                .cloned()
                .unwrap_or_else(|| json!("cos agent setup text"));
            (false, error, fix, parsed)
        }
    };

    Ok(json!({
        "ready": ready_ok,
        "ready_reason": ready_reason,
        "readiness_error": readiness_error,
        "fix": fix,
        "provider": cfg.provider,
        "model": cfg.model,
        "key_source": key_source,
        "credential_pool": {
            "declared": llm::credential_pool::Pool::is_declared(cfg),
            "credential_names": cfg.api_key_credentials,
            "environment_variables": cfg.api_key_envs,
            "strategy": cfg.pool_strategy,
            "cooldown_secs": cfg.pool_cooldown_secs,
        },
        "needs_credential": setup::provider_needs_credential(&cfg.provider),
        "config_path": setup::config_path().display().to_string(),
        "last_session": last_session,
        "daemon": daemon,
        "hint": "for the full provider/tools/skills/usage report, run `cos agent doctor`",
    }))
}

pub(super) fn providers_cmd(args: &[String]) -> Result<Value, String> {
    let mut filter_names: Option<Vec<String>> = None;
    let mut probe_credentials = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--names" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--names needs a comma list".to_string())?;
                filter_names = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
                i += 2;
            }
            "--probe-credentials" => {
                probe_credentials = true;
                i += 1;
            }
            other => {
                return Err(format!(
                    "unknown providers arg: {other}. try: --names <a,b,c> | --probe-credentials"
                ));
            }
        }
    }

    let cfg = crate::config::current_snapshot();
    let active = cfg.agent.provider.clone();
    let active_model = if cfg.agent.model.is_empty() {
        "stub-model".to_string()
    } else {
        cfg.agent.model.clone()
    };

    let mut entries = Vec::new();
    for &name in llm::available_providers().iter() {
        if let Some(filter) = filter_names.as_ref() {
            if !filter.iter().any(|n| n == name) {
                continue;
            }
        }

        let canonical_env = canonical_env_for_provider(name);
        let canonical_credential = canonical_credential_for_provider(name);
        let is_active = name == active;

        // Use the user's actual agent config for the active provider,
        // a synthetic canonical-name config for the others.
        let probe_cfg = if is_active {
            cfg.agent.clone()
        } else {
            crate::config::AgentConfig {
                provider: name.to_string(),
                api_key_credential: canonical_credential.map(String::from),
                api_key_env: canonical_env.map(String::from),
                ..Default::default()
            }
        };

        let (configured, configuration_error) =
            provider_build_status(name, &active_model, &probe_cfg);

        let env_present = canonical_env
            .map(|e| std::env::var(e).map(|v| !v.is_empty()).unwrap_or(false))
            .unwrap_or(false);

        let credential_present = if probe_credentials {
            canonical_credential
                .map(|c| {
                    crate::credential::try_load(c, "agent")
                        .map(|x| x.is_some())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        } else {
            false
        };

        // Approximate pool size from declared cfg (without resolving
        // each entry — that would require building the pool again
        // here and round-tripping lock contention). This is the count
        // of *declared* sources for the active provider, or 0 for
        // others (synthesised configs have no plural fields).
        let pool_declared_keys = if is_active {
            cfg.agent.api_key_credentials.len() + cfg.agent.api_key_envs.len()
        } else {
            0
        };
        let pool_strategy = if is_active && pool_declared_keys > 0 {
            Some(cfg.agent.pool_strategy.as_str())
        } else {
            None
        };

        entries.push(json!({
            "name": name,
            "active": is_active,
            "configured": configured,
            "default_base_url": default_base_url_for_provider(name),
            "env": canonical_env,
            "env_present": env_present,
            "credential": canonical_credential,
            "credential_present": credential_present,
            "key_required": canonical_env.is_some(),
            "pool_declared_keys": pool_declared_keys,
            "pool_strategy": pool_strategy,
            "configuration_error": configuration_error,
        }));
    }

    let active_configured = entries.iter().any(|entry| {
        entry.get("active") == Some(&Value::Bool(true))
            && entry.get("configured") == Some(&Value::Bool(true))
    });
    let active_configuration_error = entries
        .iter()
        .find(|entry| entry.get("active") == Some(&Value::Bool(true)))
        .and_then(|entry| entry.get("configuration_error"))
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "active": active,
        "active_model": cfg.agent.model.clone(),
        "active_configured": active_configured,
        "active_configuration_error": active_configuration_error,
        "probe_credentials": probe_credentials,
        "providers": entries,
        "count": entries.len(),
    }))
}

fn provider_build_status(
    name: &str,
    model: &str,
    cfg: &crate::config::AgentConfig,
) -> (bool, Value) {
    match llm::registry::build(name, model, cfg) {
        Ok(provider) => (provider.is_configured(), Value::Null),
        Err(error) => (false, setup::provider_configuration_error(cfg, &error)),
    }
}

/// `cos agent provider-doctor [--names <a,b,c>] [--probe-network]
/// [--timeout <secs>]`
///
/// Static config check + optional one-shot live ping of the active
/// LLM provider. Wraps [`providers_cmd`]'s output (env_present /
/// credential_present / configured per provider) and adds a `doctor`
/// section with the probe verdict.
///
/// **Probe target**: only the **active** provider (configured in
/// `[agent].provider`). Non-active probes are skipped because we
/// don't have a known "default cheap model" for non-active providers
/// — `Provider::supported_models()` typically echoes the configured
/// model — and guessing one (e.g. `gpt-4o-mini`) would silently
/// break when the user has another model configured.
///
/// **Skipped providers** (active but unprobeable): `mock` (pointless),
/// `llama_local` (would force a heavy GGUF load + RAM allocation,
/// surprising side effect for a "doctor" command).
///
/// **Probe shape**: minimal `chat()` request — one user message
/// (`"Reply with the single word OK."`), `max_tokens: Some(16)`. No
/// temperature / top_p / tools — those knobs cause false-negative
/// rejection on some providers/models even though basic chat works.
/// Treats any successful `chat()` round-trip as success regardless
/// of literal content; `excerpt` is informational only.
///
/// **Timeouts**: `--timeout <secs>` (default 30s) wraps the future
/// in `tokio::time::timeout`. NOTE: this is independent of the
/// provider's own `request_timeout` (set on the underlying
/// `reqwest::Client` from `AgentConfig.request_timeout`); the
/// effective ceiling is `min(--timeout, AgentConfig.request_timeout)`.
/// We surface both as `probe_timeout_secs` /
/// `provider_request_timeout_secs` to make the asymmetry visible.
///
/// **Secret hygiene**: every error/excerpt string emitted goes
/// through [`crate::agent::safety::redact::Redactor::default_set`]
/// before serialisation. `LlmError::Transport(reqwest)` can include
/// URLs (and users sometimes embed credentials in `base_url`);
/// upstream provider error text can echo Authorization headers in
/// rare cases. Always-redact > regret-later.
///
/// **Structured failure**: probe verdicts include `error_kind` —
/// one of `auth | rate_limited | not_configured | invalid_request
/// | transport | provider | parse | stream | internal | timeout`
/// — so callers can branch on the cause programmatically rather
/// than parsing redacted prose.
pub(super) fn provider_doctor_cmd(args: &[String]) -> Result<Value, String> {
    let mut probe_network = false;
    let mut timeout_secs: u64 = 30;
    let mut filter_names: Option<Vec<String>> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--names" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--names needs a comma list".to_string())?;
                filter_names = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
                i += 2;
            }
            "--probe-network" => {
                probe_network = true;
                i += 1;
            }
            "--timeout" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--timeout needs <secs>".to_string())?;
                timeout_secs = raw
                    .parse::<u64>()
                    .map_err(|_| format!("--timeout must be a positive integer (got '{raw}')"))?;
                if timeout_secs == 0 {
                    return Err("--timeout must be > 0".into());
                }
                i += 2;
            }
            other => {
                return Err(format!(
                    "unknown provider-doctor arg: {other}. try: --names <a,b,c> | --probe-network | --timeout <secs>"
                ));
            }
        }
    }

    // Re-use the static check by forwarding the relevant flags.
    // Always probe credentials in doctor mode (cheap; users running
    // doctor want a complete view).
    let mut static_args: Vec<String> = vec!["--probe-credentials".into()];
    if let Some(names) = filter_names.as_ref() {
        static_args.push("--names".into());
        static_args.push(names.join(","));
    }
    let mut out = providers_cmd(&static_args)?;

    let cfg = crate::config::current_snapshot();
    let active_name = cfg.agent.provider.clone();
    let active_in_scope = filter_names
        .as_ref()
        .map(|f| f.iter().any(|n| n == &active_name))
        .unwrap_or(true);

    let probe_value = if !probe_network {
        json!({
            "attempted": false,
            "reason": "static check only — pass --probe-network to issue a one-shot live ping",
        })
    } else if active_name.is_empty() {
        json!({
            "attempted": false,
            "reason": "no text-model provider configured — run `cos agent setup text` first (probe needs an active provider)",
        })
    } else if !active_in_scope {
        json!({
            "attempted": false,
            "reason": format!(
                "active provider '{active_name}' filtered out by --names; doctor probes only the active provider"
            ),
        })
    } else if active_name == "mock" {
        json!({
            "attempted": false,
            "reason": "mock provider: probe is meaningless (no upstream)",
        })
    } else if active_name == "llama_local" {
        json!({
            "attempted": false,
            "reason": "llama_local provider: probe is skipped — would force a GGUF load + RAM allocation, surprising side effect for a doctor command. Use 'cos model load' + 'cos agent ask' to validate end-to-end.",
        })
    } else {
        run_active_provider_probe(&active_name, &cfg.agent, timeout_secs)
    };

    // Surface the asymmetry between our probe wrapper timeout and
    // the provider's own request timeout — the effective ceiling is
    // min of the two.
    let provider_request_timeout = cfg.agent.request_timeout;

    out["doctor"] = json!({
        "active": active_name,
        "active_in_scope": active_in_scope,
        "probe_network": probe_network,
        "probe_timeout_secs": timeout_secs,
        "provider_request_timeout_secs": provider_request_timeout,
        "effective_timeout_secs": std::cmp::min(timeout_secs, provider_request_timeout),
        "active_probe": probe_value,
    });
    Ok(out)
}

/// Run the live one-shot ping for the active provider. Builds a
/// fresh provider instance (no shared state with concurrent
/// commands), spins up a single-thread Tokio runtime, and reports
/// a structured verdict. All error/excerpt strings are redacted.
pub(super) fn run_active_provider_probe(
    name: &str,
    agent_cfg: &crate::config::AgentConfig,
    timeout_secs: u64,
) -> Value {
    use crate::agent::llm::types::{ChatRequest, ContentBlock, Message};
    use crate::agent::safety::redact::Redactor;

    let model = if agent_cfg.model.is_empty() {
        "stub-model".to_string()
    } else {
        agent_cfg.model.clone()
    };
    let redactor = Redactor::default_set();

    let provider = match llm::registry::build(name, &model, agent_cfg) {
        Ok(p) => p,
        Err(e) => {
            return json!({
                "attempted": false,
                "reason": redactor.redact(&format!("provider build failed: {e}")),
                "error_kind": llm_error_kind(&e),
            });
        }
    };
    // NOTE: we deliberately do NOT wrap with `ai::gate::wrap_for_system`
    // here. The probe is an OS-internal diagnostic — it's the kernel
    // calling its own LLM stack to confirm the user's freshly-typed
    // configuration works. It is NOT an app-→AI call, so it should
    // not consume the system-agent budget bucket and it must not be
    // gated by the caps system (`cos agent setup` is typically run
    // from a user TTY with no upstream session, so requiring
    // `COS_SESSION` here would make the post-setup probe always fail
    // with "Permission denied (no active session)").

    let configured = provider.is_configured();
    let req = ChatRequest {
        model: model.clone(),
        messages: vec![Message::user_text("Reply with the single word OK.")],
        system: None,
        tools: Vec::new(),
        tool_choice: crate::agent::llm::types::ToolChoice::Auto,
        max_tokens: Some(16),
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "attempted": false,
                "reason": redactor.redact(&format!("tokio runtime: {e}")),
                "error_kind": "internal",
            });
        }
    };

    let timeout = std::time::Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    let result =
        runtime.block_on(async move { tokio::time::timeout(timeout, provider.chat(req)).await });
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Err(_elapsed) => json!({
            "attempted": true,
            "ok": false,
            "timed_out": true,
            "duration_ms": duration_ms,
            "error_kind": "timeout",
            "error_message": format!("probe timed out after {timeout_secs}s"),
            "configured_at_build": configured,
        }),
        Ok(Err(e)) => {
            let kind = llm_error_kind(&e);
            let mut entry = json!({
                "attempted": true,
                "ok": false,
                "timed_out": false,
                "duration_ms": duration_ms,
                "error_kind": kind,
                "error_message": redactor.redact(&e.to_string()),
                "configured_at_build": configured,
            });
            // Surface specific structured fields for the provider/rate-limited variants.
            match &e {
                llm::LlmError::Provider { status, .. } => {
                    entry["status"] = json!(status);
                }
                llm::LlmError::RateLimited { retry_after_ms } => {
                    entry["retry_after_ms"] = json!(retry_after_ms);
                }
                _ => {}
            }
            entry
        }
        Ok(Ok(resp)) => {
            let raw_text: String = resp
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let raw_clip: String = raw_text.chars().take(80).collect();
            let excerpt = redactor.redact(&raw_clip);
            json!({
                "attempted": true,
                "ok": true,
                "timed_out": false,
                "duration_ms": duration_ms,
                "model": resp.model,
                "input_tokens": resp.usage.input_tokens,
                "output_tokens": resp.usage.output_tokens,
                "finish_reason": match resp.finish_reason {
                    crate::agent::llm::types::FinishReason::Stop => "stop",
                    crate::agent::llm::types::FinishReason::Length => "length",
                    crate::agent::llm::types::FinishReason::ToolUse => "tool_use",
                    crate::agent::llm::types::FinishReason::Refusal => "refusal",
                    crate::agent::llm::types::FinishReason::ContentFilter => "content_filter",
                    crate::agent::llm::types::FinishReason::Other => "other",
                },
                "excerpt": excerpt,
                "configured_at_build": configured,
            })
        }
    }
}

/// Map an `LlmError` to a stable string tag for the doctor JSON
/// output. The probe-network UI branches on this tag, so don't
/// rename existing variants without considering callers.
fn llm_error_kind(e: &llm::LlmError) -> &'static str {
    match e {
        llm::LlmError::NotConfigured(_) => "not_configured",
        llm::LlmError::InvalidRequest(_) => "invalid_request",
        llm::LlmError::Transport(_) => "transport",
        llm::LlmError::Provider { .. } => "provider",
        llm::LlmError::RateLimited { .. } => "rate_limited",
        llm::LlmError::Auth => "auth",
        llm::LlmError::CredentialStore { .. } => "credential_store",
        llm::LlmError::Parse(_) => "parse",
        llm::LlmError::Stream(_) => "stream",
        llm::LlmError::Internal(_) => "internal",
        // Added by HIGH-3/MEDIUM-12 fix: upstream returned a syntactically
        // malformed payload (bad JSON in SSE, oversized headers, etc.).
        // Distinct from `parse` (which we used for any decode failure)
        // because here the bug is on the provider's side, not in the
        // request we built.
        llm::LlmError::UpstreamMalformed(_) => "upstream_malformed",
    }
}

/// Canonical env var the binary documents per provider alias.
/// Returns `None` for providers that don't use an API key (mock,
/// llama_local, ollama).
fn canonical_env_for_provider(name: &str) -> Option<&'static str> {
    match name {
        "openai" => Some("OPENAI_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        // Bedrock uses three env vars (access key + secret + optional
        // session token). We surface AWS_ACCESS_KEY_ID as the
        // "primary" one for the env_present indicator — having the
        // access key without the secret is useless, but having the
        // access key absent is a definitive "not configured" signal.
        "bedrock" => Some("AWS_ACCESS_KEY_ID"),
        // Local/no-auth providers.
        "ollama" | "mock" | "llama_local" => None,
        _ => None,
    }
}

/// Canonical credential name (in the `agent` namespace) per provider
/// alias. Mirrors `canonical_env_for_provider` but for the
/// credential store. `None` for providers that never need a key OR
/// for providers (like Bedrock) whose credential model doesn't fit
/// a single name — Bedrock uses `aws_access_key_credential` /
/// `aws_secret_key_credential` / `aws_session_token_credential`
/// independently, so there's no one-name-fits-all.
fn canonical_credential_for_provider(name: &str) -> Option<&'static str> {
    match name {
        "openai" => Some("openai"),
        "xai" => Some("xai"),
        "deepseek" => Some("deepseek"),
        "openrouter" => Some("openrouter"),
        "anthropic" => Some("anthropic"),
        "gemini" => Some("gemini"),
        "ollama" | "mock" | "llama_local" | "bedrock" => None,
        _ => None,
    }
}

/// Default base URL per provider alias when no override is set.
/// Helps users see what they'd hit out of the box.
fn default_base_url_for_provider(name: &str) -> Option<&'static str> {
    if llm::providers::openai_compat::is_alias(name) {
        Some(llm::providers::openai_compat::default_base_url_for(name))
    } else if name == "anthropic" {
        Some("https://api.anthropic.com/v1")
    } else if name == "gemini" {
        Some("https://generativelanguage.googleapis.com/v1beta")
    } else if name == "bedrock" {
        // Region-templated. We surface the template so users see
        // which region to pin via [agent].aws_region.
        Some("https://bedrock-runtime.{region}.amazonaws.com (region-derived)")
    } else if name == "llama_local" {
        Some("local: file path via AgentConfig.model")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/provider_commands.rs"
    ));
}

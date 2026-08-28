use serde_json::{json, Value};

/// `cos agent llm <providers|models|model|cost>`
///
/// Read-only inspection of the built-in
/// [`crate::agent::llm::metadata`] table — the static registry of
/// known LLM models, their context windows, capabilities, and
/// per-million-token pricing. Useful for cross-checking pricing
/// against an invoice, picking a model from the CLI without leaving
/// the terminal, or scripting a "what does this model support?"
/// guard before issuing a `cos agent ask`.
///
/// All data lives in the binary; no network or file IO is involved.
pub(super) fn llm_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::llm::metadata;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("providers");
    match sub {
        "providers" => {
            let providers: Vec<Value> = metadata::known_providers()
                .into_iter()
                .map(|name| {
                    let count = metadata::list_for_provider(name).len();
                    json!({"name": name, "models": count})
                })
                .collect();
            Ok(json!({
                "count": providers.len(),
                "total_entries": metadata::entry_count(),
                "providers": providers,
            }))
        }
        "models" => {
            let mut provider: Option<String> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--provider" => {
                        provider = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--provider needs a name".to_string())?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown flag for `llm models`: {other}")),
                }
            }
            let entries: Vec<&'static metadata::ModelMetadata> = match &provider {
                Some(p) => metadata::list_for_provider(p),
                None => metadata::known_providers()
                    .into_iter()
                    .flat_map(metadata::list_for_provider)
                    .collect(),
            };
            let models: Vec<Value> = entries.iter().map(|m| model_to_json(m)).collect();
            Ok(json!({
                "filter_provider": provider,
                "count": models.len(),
                "models": models,
            }))
        }
        "model" => {
            let name = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent llm model <name>".to_string())?;
            let m = metadata::lookup(&name).ok_or_else(|| format!("unknown model: {name}"))?;
            Ok(model_to_json(m))
        }
        other => Err(format!(
            "unknown llm subcommand: {other}. try: providers | models [--provider X] | model <name>"
        )),
    }
}

fn model_to_json(m: &crate::agent::llm::metadata::ModelMetadata) -> Value {
    json!({
        "name": m.name,
        "provider": m.provider,
        "context_window": m.context_window,
        "max_output_tokens": m.max_output_tokens,
        "supports_tools": m.supports_tools,
        "supports_vision": m.supports_vision,
        "supports_streaming": m.supports_streaming,
    })
}

/// `cos agent compress [show-config|check --file <jsonl> [...]]`
///
/// Inspect the context-window compressor without invoking it. Two
/// surfaces:
///
/// - `show-config` — dump the default `CompressorConfig` so callers
///   know where the trigger / target / keep-tail / summary-max budgets
///   currently sit.
///
/// - `check` — load a JSONL file (one `Message` per line) plus an
///   optional system prompt, run `estimate_total_tokens` on it, and
///   report whether the total clears the configured trigger and how
///   far over the target budget it would land. Useful for capacity
///   planning ("would this conversation force a summarisation?")
///   without spending API tokens on a real `LlmCompressor` round-trip.
///
/// `--trigger / --target / --keep-tail / --summary-max` override the
/// default `CompressorConfig` budgets in-place so the same recorded
/// conversation can be inspected against multiple budget profiles.
pub(super) fn compress_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::compressor::{
        estimate_message_tokens, estimate_text_tokens, estimate_total_tokens, CompressorConfig,
    };
    use crate::agent::llm::types::{Message, Role};

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show-config");
    match sub {
        "show-config" => {
            let cfg = CompressorConfig::default();
            Ok(json!({
                "target_tokens": cfg.target_tokens,
                "trigger_tokens": cfg.trigger_tokens,
                "keep_tail_tokens": cfg.keep_tail_tokens,
                "summary_max_tokens": cfg.summary_max_tokens,
            }))
        }
        "check" => {
            let mut file: Option<String> = None;
            let mut system_inline: Option<String> = None;
            let mut system_file: Option<String> = None;
            let mut cfg = CompressorConfig::default();
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--file" => {
                        file = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--file needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--system" => {
                        system_inline = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--system needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--system-file" => {
                        system_file = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--system-file needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--trigger" => {
                        cfg.trigger_tokens = parse_u32_arg(args.get(i + 1), "--trigger")?;
                        i += 2;
                    }
                    "--target" => {
                        cfg.target_tokens = parse_u32_arg(args.get(i + 1), "--target")?;
                        i += 2;
                    }
                    "--keep-tail" | "--keep_tail" => {
                        cfg.keep_tail_tokens = parse_u32_arg(args.get(i + 1), "--keep-tail")?;
                        i += 2;
                    }
                    "--summary-max" | "--summary_max" => {
                        cfg.summary_max_tokens = parse_u32_arg(args.get(i + 1), "--summary-max")?;
                        i += 2;
                    }
                    other => {
                        return Err(format!("unknown compress check flag: {other}"));
                    }
                }
            }

            let path = file.ok_or_else(|| "--file required".to_string())?;
            let raw = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
            let mut messages: Vec<Message> = Vec::new();
            for (line_no, line) in raw.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let msg: Message = serde_json::from_str(trimmed)
                    .map_err(|e| format!("parse line {} of {}: {}", line_no + 1, path, e))?;
                messages.push(msg);
            }

            let system = match (system_inline, system_file) {
                (Some(_), Some(_)) => {
                    return Err("--system and --system-file are mutually exclusive".into());
                }
                (Some(s), None) => Some(s),
                (None, Some(p)) => {
                    Some(std::fs::read_to_string(&p).map_err(|e| format!("read {p}: {e}"))?)
                }
                (None, None) => None,
            };

            let system_tokens = system.as_deref().map(estimate_text_tokens).unwrap_or(0);
            let mut role_counts = std::collections::BTreeMap::<&str, u64>::new();
            let mut role_tokens = std::collections::BTreeMap::<&str, u32>::new();
            let mut per_message: Vec<serde_json::Value> = Vec::with_capacity(messages.len());
            for (idx, msg) in messages.iter().enumerate() {
                let role = match msg.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                let toks = estimate_message_tokens(msg);
                *role_counts.entry(role).or_default() += 1;
                *role_tokens.entry(role).or_default() = role_tokens
                    .get(role)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(toks);
                per_message.push(json!({
                    "index": idx,
                    "role": role,
                    "blocks": msg.content.len(),
                    "estimated_tokens": toks,
                }));
            }
            let total = estimate_total_tokens(system.as_deref(), &messages);
            let would_trigger = total >= cfg.trigger_tokens;
            let over_target = total.saturating_sub(cfg.target_tokens);

            Ok(json!({
                "config": {
                    "target_tokens": cfg.target_tokens,
                    "trigger_tokens": cfg.trigger_tokens,
                    "keep_tail_tokens": cfg.keep_tail_tokens,
                    "summary_max_tokens": cfg.summary_max_tokens,
                },
                "system_tokens": system_tokens,
                "message_count": messages.len(),
                "messages_tokens": total.saturating_sub(system_tokens),
                "total_tokens": total,
                "would_trigger": would_trigger,
                "over_target": over_target,
                "by_role": {
                    "counts": role_counts,
                    "tokens": role_tokens,
                },
                "messages": per_message,
            }))
        }
        other => Err(format!(
            "unknown compress subcommand: {other}. try: show-config | check --file <jsonl>"
        )),
    }
}

fn parse_u32_arg(raw: Option<&String>, flag: &str) -> Result<u32, String> {
    let s = raw.ok_or_else(|| format!("{flag} needs a value"))?;
    s.parse::<u32>().map_err(|e| format!("{flag}: {e}"))
}

/// `cos agent aux [show|ask --prompt <text> [--system <text>] [--max-tokens N]]`
///
/// Inspect or invoke the auxiliary LLM client. The auxiliary path
/// exists so lightweight subtasks (title generation, classification,
/// summarisation) can route to a cheap secondary model instead of
/// burning flagship tokens. Configuration lives in
/// `AgentConfig::auxiliary_*`.
///
/// `show` reports the resolved auxiliary settings (provider / model
/// / max_tokens / temperature / configured?) without making any
/// network calls. `ask` actually invokes `AuxiliaryClient::ask`
/// against the configured provider — useful as a smoke test that the
/// cheap model is reachable and that credentials route correctly.
pub(super) fn aux_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "" => {
            let cfg = &crate::config::get().agent;
            let aux_built = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg);
            let (configured, build_error) = match &aux_built {
                Ok(Some(_)) => (true, None),
                Ok(None) => (false, None),
                Err(e) => (false, Some(e.to_string())),
            };
            Ok(json!({
                "configured": configured,
                "provider": cfg.auxiliary_provider,
                "model": cfg.auxiliary_model,
                "max_tokens": cfg.auxiliary_max_tokens,
                "temperature": cfg.auxiliary_temperature,
                "build_error": build_error,
                "note": "Auxiliary calls share base_url / credentials with the primary provider unless the underlying builder honours its own env vars.",
            }))
        }
        "ask" => {
            let mut prompt: Option<String> = None;
            let mut system: Option<String> = None;
            let mut max_tokens_override: Option<u32> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--prompt" => {
                        prompt = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--prompt needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--system" => {
                        system = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--system needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--max-tokens" | "--max_tokens" => {
                        max_tokens_override =
                            Some(parse_u32_arg(args.get(i + 1), "--max-tokens")?);
                        i += 2;
                    }
                    other => return Err(format!("unknown aux ask flag: {other}")),
                }
            }
            let prompt = prompt.ok_or_else(|| "--prompt required".to_string())?;
            let cfg = &crate::config::get().agent;
            let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    "auxiliary client is not configured (set agent.auxiliary_provider + auxiliary_model in config)"
                        .to_string()
                })?;
            // Apply per-call max_tokens override by rebuilding a
            // fresh AuxiliaryClient with the overridden config. The
            // underlying provider Arc is reused.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let used_max_tokens = max_tokens_override.unwrap_or(aux.config().max_tokens);
            let answer = runtime
                .block_on(aux.ask(system.as_deref(), &prompt))
                .map_err(|e| format!("aux ask: {e}"))?;
            Ok(json!({
                "ok": true,
                "provider": aux.provider_name(),
                "model": aux.config().model,
                "max_tokens": used_max_tokens,
                "answer": answer,
            }))
        }
        other => Err(format!(
            "unknown aux subcommand: {other}. try: show | ask --prompt <text> [--system <text>] [--max-tokens N]"
        )),
    }
}

/// `cos agent retry [show|schedule [--attempts N]]`
///
/// Surface for the LLM-call retry policy resolved from the agent
/// config via [`crate::agent::runtime::loop_::retry_policy_from_cfg`].
///
/// `show` reports whether retries are enabled and the resolved
/// `RetryPolicy` (max_attempts / base_ms / max_ms / jitter), or
/// reports `enabled: false` when the helper returns `None`.
///
/// `schedule` previews the back-off delays the policy would emit per
/// attempt (1-indexed, exclusive of the first call). Useful for
/// capacity planning ("if every retry fires, how long until we give
/// up?") and for verifying that `retry_max_attempts` matches what's
/// in config without round-tripping a live request.
///
/// Because `RetryPolicy::delay_for` adds jitter when configured, the
/// schedule is non-deterministic when `jitter == true`; the output
/// includes the per-attempt delay AND the cap (`max_ms`) so callers
/// can compute worst-case bounds.
pub(super) fn retry_cmd(args: &[String]) -> Result<Value, String> {
    let cfg = &crate::config::get().agent;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "" => {
            let policy = crate::agent::runtime::loop_::retry_policy_from_cfg(cfg);
            match policy {
                Some(p) => Ok(json!({
                    "enabled": true,
                    "max_attempts": p.max_attempts,
                    "base_ms": p.base_ms,
                    "max_ms": p.max_ms,
                    "jitter": p.jitter,
                    "config_retry_enabled": cfg.retry_enabled,
                    "config_retry_max_attempts": cfg.retry_max_attempts,
                })),
                None => Ok(json!({
                    "enabled": false,
                    "config_retry_enabled": cfg.retry_enabled,
                    "config_retry_max_attempts": cfg.retry_max_attempts,
                    "note": "retry_enabled is false OR retry_max_attempts < 2; only one attempt will fire on transient failure.",
                })),
            }
        }
        "schedule" => {
            let mut override_attempts: Option<u32> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--attempts" => {
                        override_attempts = Some(parse_u32_arg(args.get(i + 1), "--attempts")?);
                        i += 2;
                    }
                    other => return Err(format!("unknown retry schedule flag: {other}")),
                }
            }
            // Use the cfg-derived policy if present; otherwise fall
            // back to a synthesised standard policy. Either way,
            // --attempts overrides max_attempts so callers can probe
            // alternate schedules without rewriting config.
            let mut policy = crate::agent::runtime::loop_::retry_policy_from_cfg(cfg)
                .unwrap_or_else(crate::agent::llm::rate_limit::RetryPolicy::standard);
            if let Some(a) = override_attempts {
                policy.max_attempts = a;
            }
            let max_attempts = policy.max_attempts.max(1);
            // delay_for(attempt) is the delay AFTER `attempt` failures
            // (1-indexed). For max_attempts = N total attempts, there
            // are N-1 inter-attempt waits.
            let mut schedule: Vec<Value> =
                Vec::with_capacity(max_attempts.saturating_sub(1) as usize);
            let mut total_min_ms: u64 = 0;
            let mut total_max_ms: u64 = 0;
            for attempt in 1..max_attempts {
                let d = policy.delay_for(attempt);
                let d_ms = d.as_millis() as u64;
                // Worst case (jitter cap = 1.0): clamped base = base * 2^(attempt-1) capped at max_ms.
                let exp = attempt.saturating_sub(1).min(20);
                let raw_base = policy
                    .base_ms
                    .saturating_mul(1u64.checked_shl(exp).unwrap_or(u64::MAX));
                let cap = raw_base.min(policy.max_ms);
                total_min_ms = total_min_ms.saturating_add(d_ms);
                total_max_ms = total_max_ms.saturating_add(cap);
                schedule.push(json!({
                    "attempt": attempt,
                    "delay_ms": d_ms,
                    "cap_ms": cap,
                }));
            }
            Ok(json!({
                "max_attempts": max_attempts,
                "base_ms": policy.base_ms,
                "max_ms": policy.max_ms,
                "jitter": policy.jitter,
                "inter_attempt_waits": schedule,
                "total_observed_ms": total_min_ms,
                "total_worst_case_ms": total_max_ms,
            }))
        }
        other => Err(format!(
            "unknown retry subcommand: {other}. try: show | schedule [--attempts N]"
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/model_commands.rs"
    ));
}

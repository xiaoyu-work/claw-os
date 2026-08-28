use serde_json::{json, Value};

/// `cos agent prompt [show|build] [--extra <path>] [--raw]`
///
/// Inspect the canonical system-prompt candidate frozen for a new session.
/// Existing sessions restore their persisted snapshot instead of rebuilding.
/// The candidate is composed by
/// [`crate::agent::prompt::build_system_prompt`] and includes:
///
///   1. Built-in scaffold (immutable in this binary).
///   2. Metadata-only installed Skill catalogue.
///   3. `MEMORY.md` and `USER.md` from the system notes store
///      (auto-loaded; capped per-file via
///      [`crate::agent::memory::notes::MAX_NOTE_CHARS_FOR_PROMPT`]).
///   4. Optional override file content from `--extra <path>`.
///
/// Useful for: debugging "why did the model behave this way?",
/// previewing a new MEMORY.md entry before committing, computing a
/// rough token budget for a new session, or capturing the candidate
/// to share in a bug report. Due nudges are reported separately because
/// they are request-local context, not part of the frozen prompt.
///
/// `--raw` returns the prompt as a single JSON string in the
/// `prompt` field (default). Without `--raw` the response also
/// includes a size breakdown and the currently due request-local context.
pub(super) fn prompt_cmd(args: &[String]) -> Result<Value, String> {
    use std::path::PathBuf;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    if sub != "show" && sub != "build" && !sub.is_empty() {
        return Err(format!(
            "unknown prompt subcommand: {sub}. try: show [--extra <path>] [--raw] | build [--extra <path>] [--raw]"
        ));
    }
    let mut extra: Option<PathBuf> = None;
    let mut raw = false;
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--extra" => {
                let p = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--extra needs a path".to_string())?;
                extra = Some(PathBuf::from(p));
                i += 2;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            other => {
                return Err(format!("unknown flag for `prompt`: {other}"));
            }
        }
    }
    let extra_ref = extra.as_deref();
    let prompt = crate::agent::prompt::build_system_prompt(extra_ref);
    let turn_context = crate::agent::prompt::build_turn_context_segments();
    let turn_context_chars: usize = turn_context
        .iter()
        .map(|segment| segment.content.chars().count())
        .sum();
    let turn_context_sources: Vec<&str> =
        turn_context.iter().map(|segment| segment.source).collect();
    if raw {
        Ok(json!({
            "prompt": prompt,
            "chars": prompt.chars().count(),
            "prompt_version": crate::agent::prompt::CANONICAL_PROMPT_VERSION,
            "scope": "new-session-candidate",
            "turn_context": turn_context.iter().map(|segment| json!({
                "source": segment.source,
                "content": segment.content,
            })).collect::<Vec<_>>(),
        }))
    } else {
        // Crude size breakdown: rebuild each piece in isolation by
        // diffing against a scaffold-only build. This is for a
        // quick visual inventory; the prompt itself is the
        // authoritative artifact.
        let scaffold_only = crate::agent::prompt::build_system_prompt(None);
        let scaffold_chars = scaffold_only.chars().count();
        let total_chars = prompt.chars().count();
        let extra_chars = if let Some(p) = extra_ref {
            std::fs::read_to_string(p)
                .map(|s| s.trim_end().chars().count())
                .unwrap_or(0)
        } else {
            0
        };
        Ok(json!({
            "prompt": prompt,
            "chars": total_chars,
            "scaffold_chars": scaffold_chars,
            "extra_path": extra.as_ref().map(|p| p.display().to_string()),
            "extra_chars": extra_chars,
            "approx_tokens": total_chars / 4,
            "prompt_version": crate::agent::prompt::CANONICAL_PROMPT_VERSION,
            "scope": "new-session-candidate",
            "turn_context_chars": turn_context_chars,
            "turn_context_sources": turn_context_sources,
        }))
    }
}

/// `cos agent think-scrub <text> [--check] [--strict]`
/// `cos agent think-scrub --file <path> [--check]`
/// `cos agent think-scrub --stdin [--check]`
///
/// Standalone interface to
/// [`crate::agent::context::think_scrub::ThinkScrubber`]. Strips
/// `<think>...</think>`, `<thinking>...</thinking>`, and
/// `<reasoning>...</reasoning>` blocks (multiline) from text.
///
/// Useful for: post-processing a transcript before pasting it into
/// a bug report, normalising responses from a reasoning model
/// before computing a diff against a non-reasoning baseline,
/// scripting "did this output contain hidden reasoning?" gates.
///
/// `--check` returns `{has_thinking: bool}` instead of scrubbing.
pub(super) fn think_scrub_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::think_scrub::ThinkScrubber;

    let (input, check) = read_text_input(args, "think-scrub")?;
    let scrubber = ThinkScrubber::new();
    if check {
        Ok(json!({
            "has_thinking": scrubber.has_thinking(&input),
            "input_chars": input.chars().count(),
        }))
    } else {
        let scrubbed = scrubber.scrub(&input);
        let changed = scrubbed != input;
        Ok(json!({
            "scrubbed": scrubbed,
            "changed": changed,
            "input_chars": input.chars().count(),
            "output_chars": scrubbed.chars().count(),
        }))
    }
}

/// `cos agent tokens <text>`
/// `cos agent tokens --file <path>`
/// `cos agent tokens --stdin`
///
/// Crude token estimate (chars / 4) as used by
/// [`crate::agent::context::compressor::estimate_text_tokens`].
/// This is the same heuristic used inside the runtime to decide
/// when to trigger context compression, so the number you see here
/// is the same number the agent uses internally.
///
/// Not a tokenizer — it's deliberately model-agnostic and biased
/// slightly high so callers don't *under*-estimate. For
/// production-grade counts, integrate a tokenizer matching your
/// target model.
pub(super) fn tokens_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::compressor::estimate_text_tokens;

    let (input, _check) = read_text_input(args, "tokens")?;
    let chars = input.chars().count();
    let bytes = input.len();
    let approx_tokens = estimate_text_tokens(&input);
    Ok(json!({
        "chars": chars,
        "bytes": bytes,
        "approx_tokens": approx_tokens,
        "method": "chars / 4 (model-agnostic heuristic; biased slightly high)",
    }))
}

/// Shared parser for the small family of "text-in / result-out"
/// agent subcommands (`redact`, `think-scrub`, `tokens`). Returns
/// `(input, check_mode)`.
///
/// Sources:
///   * `--file <path>` — read file content.
///   * `--stdin` — read all of stdin.
///   * positional args — joined with spaces (so the shell-natural
///     `cos agent tokens hello world` works without quoting).
///
/// `--check` is honoured by callers that have a "detect-only" mode;
/// `tokens_cmd` ignores it.
fn read_text_input(args: &[String], cmd: &str) -> Result<(String, bool), String> {
    let mut from_stdin = false;
    let mut from_file: Option<String> = None;
    let mut check = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--stdin" => {
                from_stdin = true;
                i += 1;
            }
            "--file" => {
                from_file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--check" => {
                check = true;
                i += 1;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    let input = if from_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("read stdin: {e}"))?;
        buf
    } else if let Some(path) = from_file {
        std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?
    } else if positional.is_empty() {
        return Err(format!(
            "usage: cos agent {cmd} <text> | --file <path> | --stdin"
        ));
    } else {
        positional.join(" ")
    };
    Ok((input, check))
}

/// `cos agent title <text> | --file <path> | --stdin [--check] [--llm]`
/// — heuristic-only by default. Strips a leading slash-command verb
/// (so `/ask hello` becomes `hello`), takes the first non-empty
/// line, and clamps to `MAX_TITLE_CHARS`. Pure function, no LLM
/// call, no IO beyond the input read.
///
/// `--llm` opts into the LLM-backed path used by `runtime::loop_`:
/// resolves the auxiliary client from
/// [`crate::agent::runtime::loop_::auxiliary_from_cfg`] and calls
/// [`crate::agent::title::generate_title`]. Errors and empty model
/// output fall back to the heuristic. If no auxiliary client is
/// configured, errs with a clear message instead of silently
/// downgrading (so the operator knows their `--llm` request didn't
/// actually use the model).
pub(super) fn title_cmd(args: &[String]) -> Result<Value, String> {
    let mut llm_mode = false;
    let mut filtered: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        if a == "--llm" {
            llm_mode = true;
        } else {
            filtered.push(a.clone());
        }
    }
    let (input, _check) = read_text_input(&filtered, "title")?;
    if !llm_mode {
        return Ok(title_heuristic_payload(&input));
    }
    let cfg = &crate::config::get().agent;
    let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
        .map_err(|e| format!("auxiliary client build failed: {e}"))?
        .ok_or_else(|| {
            "auxiliary client is not configured; set agent.auxiliary_provider + auxiliary_model in config or drop --llm"
                .to_string()
        })?;
    title_cmd_with_aux(&input, Some(&aux))
}

/// Inner helper: render either the heuristic title or call the LLM
/// path against a caller-supplied auxiliary client. Extracted so
/// tests can drive the LLM path with a `MockProvider`-backed
/// `AuxiliaryClient` without depending on global config state.
fn title_cmd_with_aux(
    input: &str,
    aux: Option<&crate::agent::llm::auxiliary::AuxiliaryClient>,
) -> Result<Value, String> {
    let Some(aux) = aux else {
        return Ok(title_heuristic_payload(input));
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let title = runtime.block_on(crate::agent::title::generate_title(Some(aux), input));
    Ok(json!({
        "title": title,
        "input_chars": input.chars().count(),
        "title_chars": title.chars().count(),
        "method": "llm",
        "provider": aux.provider_name(),
        "model": aux.config().model,
    }))
}

fn title_heuristic_payload(input: &str) -> Value {
    let title = crate::agent::title::clamp(&crate::agent::title::heuristic(input));
    json!({
        "title": title,
        "input_chars": input.chars().count(),
        "title_chars": title.chars().count(),
        "method": "heuristic",
    })
}

/// `cos agent summarise <text> | --file <path> | --stdin [--max N]`
/// — heuristic-only summary: take the first sentence (terminated by
/// `.`/`!`/`?` followed by whitespace or EOS) and clamp to `--max`
/// chars (default 200, matching the runtime's compressor default).
/// Pure function, no LLM call. As with `title`, the async
/// `agent::summarise::summarise` is the LLM-backed path used by the
/// runtime when an auxiliary client is configured; this CLI surfaces
/// the deterministic fallback for testing or for cheap one-offs.
///
/// Aliased as `cos agent summarize` (US spelling) so muscle memory
/// from either spelling works.
pub(super) fn summarise_cmd(args: &[String]) -> Result<Value, String> {
    let mut max_chars: usize = 200;
    let mut llm_mode = false;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--max" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max needs a number".to_string())?;
                max_chars = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--max: invalid u64: {e}"))?;
                i += 2;
            }
            "--llm" => {
                llm_mode = true;
                i += 1;
            }
            _ => {
                filtered.push(args[i].clone());
                i += 1;
            }
        }
    }
    let (input, _check) = read_text_input(&filtered, "summarise")?;
    if !llm_mode {
        return Ok(summarise_heuristic_payload(&input, max_chars));
    }
    let cfg = &crate::config::get().agent;
    let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
        .map_err(|e| format!("auxiliary client build failed: {e}"))?
        .ok_or_else(|| {
            "auxiliary client is not configured; set agent.auxiliary_provider + auxiliary_model in config or drop --llm"
                .to_string()
        })?;
    summarise_cmd_with_aux(&input, max_chars, Some(&aux))
}

/// Inner helper used by tests and by the live `--llm` path. When
/// `aux` is `None` the heuristic payload is returned unchanged so
/// callers always get a stable JSON shape.
fn summarise_cmd_with_aux(
    input: &str,
    max_chars: usize,
    aux: Option<&crate::agent::llm::auxiliary::AuxiliaryClient>,
) -> Result<Value, String> {
    let Some(aux) = aux else {
        return Ok(summarise_heuristic_payload(input, max_chars));
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let summary = runtime.block_on(crate::agent::summarise::summarise(
        Some(aux),
        input,
        max_chars,
    ));
    Ok(json!({
        "summary": summary,
        "input_chars": input.chars().count(),
        "summary_chars": summary.chars().count(),
        "max_chars": max_chars,
        "method": "llm",
        "provider": aux.provider_name(),
        "model": aux.config().model,
    }))
}

fn summarise_heuristic_payload(input: &str, max_chars: usize) -> Value {
    let raw = crate::agent::summarise::heuristic(input);
    let summary = crate::agent::summarise::clamp(&raw, max_chars);
    json!({
        "summary": summary,
        "input_chars": input.chars().count(),
        "summary_chars": summary.chars().count(),
        "max_chars": max_chars,
        "clamped": raw.chars().count() > max_chars,
        "method": "heuristic",
    })
}

/// `cos agent classify <reply> --labels <a,b,c> | --file <path> | --stdin`
/// — match a (typically LLM-generated) reply string against a label
/// set using `match_label`'s case-insensitive + punctuation-tolerant
/// rules. Returns `{matched: <label> | null, labels: [...], reply}`.
/// Useful for testing prompt designs without spending tokens (you
/// can hand-craft a hypothetical reply and confirm the parser would
/// accept it).
pub(super) fn classify_cmd(args: &[String]) -> Result<Value, String> {
    let mut labels_raw: Option<String> = None;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        if args[i].as_str() == "--labels" {
            labels_raw = Some(
                args.get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--labels needs a comma list".to_string())?,
            );
            i += 2;
        } else {
            filtered.push(args[i].clone());
            i += 1;
        }
    }
    let labels_str = labels_raw
        .ok_or_else(|| "usage: cos agent classify <reply> --labels <a,b,c>".to_string())?;
    let labels: Vec<String> = labels_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if labels.is_empty() {
        return Err("--labels: at least one non-empty label required".into());
    }
    let (reply, _check) = read_text_input(&filtered, "classify")?;
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let matched = crate::agent::classify::match_label(&reply, &label_refs);
    Ok(json!({
        "matched": matched,
        "labels": labels,
        "reply": reply,
        "reply_chars": reply.chars().count(),
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/text_commands.rs"
    ));
}

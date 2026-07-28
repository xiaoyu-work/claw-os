//! `cos ai chat` — App-facing single-shot LLM call.
//!
//! This is the **only** sanctioned entry point for an installed
//! third-party App to reach a model. The App-gated path lives under
//! its own `cos ai …` namespace deliberately: it shares no CLI
//! surface with `cos agent` (the kernel's own Agent product, with
//! loop / memory / skills / hooks / recall). Apps get raw LLM
//! access plus the App–AI Gate; they do not get the Agent.
//!
//! The gate ([`crate::ai::gate`]) enforces, in order: modality
//! derivation, capability check, manifest model glob + prompt origin
//! allowlist, per-app monthly budget, safety profile, audit. The
//! **modality** (and therefore the caps verb required) is derived
//! from the request shape — Apps never pass a verb directly:
//!
//!   - `--prompt` (text only)                       → `ai.chat`
//!   - `--prompt` + `--origin external-content`     → `ai.chat.untrusted`
//!   - `--embed --prompt <text>`                    → `ai.embed`
//!   - `--prompt <text> --image-output <path>`      → `ai.image.generate`
//!   - `--image-input <path>`                       → `ai.image.analyze`
//!   - `--image-input <p> --prompt <q>`             → `ai.vision.analyze`
//!   - `--prompt <text> --audio-output <path>`      → `ai.audio.tts`
//!   - `--audio-input <path>`                       → `ai.audio.stt`
//!   - `--prompt <text> --video-output <path>`      → `ai.video.generate`
//!   - `--video-input <path>`                       → `ai.video.analyze`
//!
//! Flags:
//!   --app <id>           App requesting the call (required).
//!   --prompt <text>      Text portion of the request (modality-dependent).
//!   --prompt-file <p>    Read prompt body from a file.
//!   --origin <kind>      trusted | user-input | external-content (default: trusted).
//!   --max-units <N>      Cap units for this call.
//!   --system <text>      Optional system prompt.
//!   --embed              Request an embedding (vector) instead of text.
//!   --image-input <p>    Image to analyse.
//!   --image-output <p>   Path the gate writes the generated image to.
//!   --audio-input <p>    Audio to transcribe.
//!   --audio-output <p>   Path the gate writes synthesised speech to.
//!   --video-input <p>    Video to analyse.
//!   --video-output <p>   Path the gate writes the generated video to.
//!   --tools <list>       Comma-separated catalog tool names to expose
//!                        to the model (e.g. `fs.read_text,kv.get`).
//!                        Each name is resolved against
//!                        `cos ai tools`; unknown names hard-deny.
//!                        The model may *propose* calls; the gate
//!                        returns them in `tool_calls[]` and never
//!                        executes them. Apps shell back via
//!                        `cos ai tool <name>` to fulfil whichever
//!                        they choose.
//!
//! Apps do **not** pick the model — the OS owner configures one
//! provider/model in `/etc/cos/agent.toml` and the gate uses it for
//! every App call.
//!
//! Identity
//! --------
//!
//! Per `docs/app-ai-integration.md` §3, an App's identity is established
//! **only** when the kernel itself spawns the App (`cos app <id> <op>`).
//! `core/src/bridge.rs` creates a registered App session, binds it to the
//! spawned PID, and injects both `COS_SESSION` and `COS_APP_ID`. A call is
//! accepted only when the env claim, registry `app_id`, and nearest App
//! process ancestry all agree with `--app`. No environment variable is
//! trusted as an identity boundary by itself.

use serde_json::{json, Value};

use super::gate;

pub fn chat_cmd(args: &[String]) -> Result<Value, String> {
    let mut app: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut prompt_file: Option<String> = None;
    let mut origin = "trusted".to_string();
    let mut max_units: Option<u64> = None;
    let mut system: Option<String> = None;
    let mut embed = false;
    let mut image_input: Option<std::path::PathBuf> = None;
    let mut image_output: Option<std::path::PathBuf> = None;
    let mut audio_input: Option<std::path::PathBuf> = None;
    let mut audio_output: Option<std::path::PathBuf> = None;
    let mut video_input: Option<std::path::PathBuf> = None;
    let mut video_output: Option<std::path::PathBuf> = None;
    let mut tools: Vec<String> = Vec::new();

    fn take_path(args: &[String], i: usize, flag: &str) -> Result<std::path::PathBuf, String> {
        args.get(i + 1)
            .cloned()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| format!("missing path for {flag}"))
    }

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                app = args.get(i + 1).cloned();
                i += 2;
            }
            "--prompt" => {
                prompt = args.get(i + 1).cloned();
                i += 2;
            }
            "--prompt-file" => {
                prompt_file = args.get(i + 1).cloned();
                i += 2;
            }
            "--origin" => {
                origin = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "missing value for --origin".to_string())?;
                i += 2;
            }
            "--max-units" => {
                max_units = Some(
                    args.get(i + 1)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| "--max-units expects an integer".to_string())?,
                );
                i += 2;
            }
            "--system" => {
                system = args.get(i + 1).cloned();
                i += 2;
            }
            "--embed" => {
                embed = true;
                i += 1;
            }
            "--image-input" => {
                image_input = Some(take_path(args, i, "--image-input")?);
                i += 2;
            }
            "--image-output" => {
                image_output = Some(take_path(args, i, "--image-output")?);
                i += 2;
            }
            "--audio-input" => {
                audio_input = Some(take_path(args, i, "--audio-input")?);
                i += 2;
            }
            "--audio-output" => {
                audio_output = Some(take_path(args, i, "--audio-output")?);
                i += 2;
            }
            "--video-input" => {
                video_input = Some(take_path(args, i, "--video-input")?);
                i += 2;
            }
            "--video-output" => {
                video_output = Some(take_path(args, i, "--video-output")?);
                i += 2;
            }
            "--tools" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "missing value for --tools".to_string())?;
                tools = parse_tools_flag(raw);
                i += 2;
            }
            other => return Err(format!("unknown flag for `cos ai chat`: {other}")),
        }
    }

    let app = app.ok_or_else(|| "--app is required".to_string())?;
    enforce_identity_for(&app)?;

    let prompt_text: Option<String> = match (prompt, prompt_file) {
        (Some(p), _) => Some(p),
        (None, Some(path)) => Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("--prompt-file {path}: {e}"))?,
        ),
        (None, None) => None,
    };

    let req = gate::ChatRequest {
        app_id: app,
        origin,
        prompt: prompt_text,
        system,
        max_units,
        embed,
        image_input,
        image_output,
        audio_input,
        audio_output,
        video_input,
        video_output,
        tools,
    };

    match gate::chat_blocking(req) {
        Ok(r) => Ok(serde_json::to_value(r).unwrap_or(json!({}))),
        Err(e) => Err(e.to_string()),
    }
}

/// Verify the caller is the App they claim to be.
///
/// `arg_app` is the `--app <id>` value parsed from the CLI; `env_app`
/// is the current process's `COS_APP_ID` claim (or `None` if unset).
/// [`enforce_identity_for`] additionally verifies the registered App
/// session and process ancestry.
///
/// A bare-process invocation (no `COS_APP_ID`) is rejected with a
/// dev-friendly hint; an env value that disagrees with `--app` is
/// rejected as cross-App impersonation.
fn enforce_identity(arg_app: &str, env_app: Option<&str>) -> Result<(), String> {
    match env_app {
        None => Err(format!(
            "`cos ai chat` must be invoked by the kernel via `cos app {arg_app} <op>`. \
             COS_APP_ID is not set, so the claimed App identity is missing."
        )),
        Some(env) if env != arg_app => Err(format!(
            "identity mismatch: --app={arg_app} but COS_APP_ID={env}. \
             An App may only request AI calls for its registered identity."
        )),
        Some(_) => Ok(()),
    }
}

/// Convenience wrapper used by sibling subcommands (`cos ai tool`)
/// that want the same identity enforcement as `cos ai chat`. Reads
/// `COS_APP_ID` from the current process env.
pub fn enforce_identity_for(arg_app: &str) -> Result<(), String> {
    enforce_identity(arg_app, std::env::var("COS_APP_ID").ok().as_deref())?;
    let session = crate::proc::current_session_info_for_caps()
        .ok_or_else(|| "App identity session is not registered".to_string())?;
    if session.app_id.as_deref() != Some(arg_app) {
        return Err(format!(
            "App identity mismatch: session `{}` is registered for {:?}, not `{arg_app}`",
            session.session_id, session.app_id
        ));
    }
    crate::caps::require(
        crate::caps::Verb::AGENT_INVOKE,
        crate::caps::Scope::name(arg_app),
    )
    .map_err(|denial| format!("App identity ancestry check failed: {}", denial.summary()))
}

/// Parse `--tools` value (e.g. `"fs.read_text,kv.get"`) into a clean
/// list. Whitespace around items is trimmed and empty segments are
/// dropped, so `"fs.read_text, , kv.get"` collapses to two entries.
fn parse_tools_flag(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_app() {
        let err = chat_cmd(&["--prompt".into(), "hi".into()]).unwrap_err();
        assert!(err.contains("--app"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_flag() {
        let err = chat_cmd(&[
            "--app".into(),
            "foo".into(),
            "--frobnicate".into(),
        ])
        .unwrap_err();
        assert!(err.contains("unknown flag"), "got: {err}");
    }

    #[test]
    fn identity_rejects_unset_env() {
        let err = enforce_identity("summarize", None).unwrap_err();
        assert!(err.contains("COS_APP_ID is not set"), "got: {err}");
        assert!(err.contains("summarize"), "got: {err}");
    }

    #[test]
    fn identity_rejects_mismatch() {
        let err = enforce_identity("summarize", Some("other-app")).unwrap_err();
        assert!(err.contains("identity mismatch"), "got: {err}");
        assert!(err.contains("--app=summarize"), "got: {err}");
        assert!(err.contains("COS_APP_ID=other-app"), "got: {err}");
    }

    #[test]
    fn identity_accepts_exact_match() {
        assert!(enforce_identity("summarize", Some("summarize")).is_ok());
    }

    #[test]
    fn identity_is_case_sensitive() {
        assert!(enforce_identity("summarize", Some("Summarize")).is_err());
    }

    #[test]
    fn parse_tools_flag_basic() {
        let v = parse_tools_flag("fs.read_text,kv.get");
        assert_eq!(v, vec!["fs.read_text".to_string(), "kv.get".to_string()]);
    }

    #[test]
    fn parse_tools_flag_trims_and_drops_empty() {
        let v = parse_tools_flag("fs.read_text,  ,kv.get , ");
        assert_eq!(v, vec!["fs.read_text".to_string(), "kv.get".to_string()]);
    }

    #[test]
    fn parse_tools_flag_empty_string_yields_empty_vec() {
        assert!(parse_tools_flag("").is_empty());
        assert!(parse_tools_flag("  ,  ").is_empty());
    }
}

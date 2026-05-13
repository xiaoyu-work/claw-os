//! `cos ai` — the App–AI Gate.
//!
//! Every app-driven LLM / image / TTS / STT / vision call passes
//! through this subsystem. The gate enforces, in order:
//!
//!   1. Capability — the calling app's session must hold the AI verb
//!      (e.g. `ai.chat`) at a `name` scope covering the requested
//!      model.
//!   2. Manifest policy — the app's manifest must declare an `ai`
//!      block; the requested model must match one of its globs, and
//!      the prompt's declared `origin` must be in its origin list.
//!   3. Budget — the per-app monthly cap (units + dollars). Reserved
//!      pre-call and finalised after the provider returns; over-cap
//!      requests are hard-denied.
//!   4. Safety — `Strict` redacts secrets in the prompt before
//!      sending it upstream. `Minimal` is audit-only.
//!   5. Audit — every accepted and denied call is logged.
//!
//! Apps **never** talk to providers directly. The frontend desktop
//! talks to apps; apps shell out to `cos ai chat`; `cos ai chat`
//! dispatches into the registered provider after every gate above
//! has cleared.
//!
//! ## What this module does NOT do
//!
//! - It does not host the per-turn agent loop. `cos agent ask` keeps
//!   its own kernel-only loop with full tool/memory wiring.
//! - It does not classify or detect prompt injection beyond routing
//!   `external-content` through the strict safety profile. The
//!   injection detector and classifier remain Phase 8 work.

pub mod budget;
pub mod gate;

use serde_json::{json, Value};

/// Dispatch a `cos ai <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "chat" => chat_cmd(args),
        "budget" => budget_cmd(args),
        other => Err(format!(
            "unknown command: {other}. try: chat | budget"
        )),
    }
}

/// `cos ai chat` — single-shot text completion routed through the
/// app-AI gate.
///
/// Flags:
///   --app <id>         App requesting the call (required).
///   --prompt <text>    User prompt (required, unless --prompt-file).
///   --prompt-file <p>  Read prompt body from a file.
///   --model <name>     Model name to use (default: app's first
///                      glob, resolved against installed providers).
///   --origin <kind>    trusted | user-input | external-content
///                      (default: trusted).
///   --verb <name>      AI verb to require (default: ai.chat). Use
///                      `ai.chat.untrusted` when origin is external.
///   --max-units <N>    Cap units for this call (default: budget
///                      remaining).
///   --system <text>    Optional system prompt.
fn chat_cmd(args: &[String]) -> Result<Value, String> {
    let mut app: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut prompt_file: Option<String> = None;
    let mut model: Option<String> = None;
    let mut origin = "trusted".to_string();
    let mut verb = "ai.chat".to_string();
    let mut max_units: Option<u64> = None;
    let mut system: Option<String> = None;

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
            "--model" => {
                model = args.get(i + 1).cloned();
                i += 2;
            }
            "--origin" => {
                origin = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "missing value for --origin".to_string())?;
                i += 2;
            }
            "--verb" => {
                verb = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "missing value for --verb".to_string())?;
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
            other => return Err(format!("unknown flag: {other}")),
        }
    }

    let app = app.ok_or_else(|| "--app is required".to_string())?;

    let prompt_text = match (prompt, prompt_file) {
        (Some(p), _) => p,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| format!("--prompt-file {path}: {e}"))?,
        (None, None) => {
            return Err("either --prompt or --prompt-file is required".to_string())
        }
    };

    let req = gate::ChatRequest {
        app_id: app,
        verb,
        model,
        origin,
        prompt: prompt_text,
        system,
        max_units,
    };

    match gate::chat_blocking(req) {
        Ok(r) => Ok(serde_json::to_value(r).unwrap_or(json!({}))),
        Err(e) => Err(e.to_string()),
    }
}

/// `cos ai budget` — inspect per-app AI spend.
///
/// Subcommands:
///   show <app>          Current period: used vs cap.
///   reset <app>         Roll over to next period (clears used).
///   history <app>       List past periods.
fn budget_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "show" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos ai budget show <app>".to_string())?;
            let store = budget::Store::open()?;
            let snap = store.current(app).map_err(|e| e.to_string())?;
            Ok(json!({
                "app": app,
                "period": snap.period,
                "units_used": snap.units_used,
                "usd_used": snap.usd_used,
            }))
        }
        "reset" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos ai budget reset <app>".to_string())?;
            let store = budget::Store::open()?;
            store.reset(app).map_err(|e| e.to_string())?;
            Ok(json!({"app": app, "reset": true}))
        }
        "history" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos ai budget history <app>".to_string())?;
            let store = budget::Store::open()?;
            let rows = store.history(app).map_err(|e| e.to_string())?;
            Ok(json!({"app": app, "history": rows}))
        }
        _ => Err("usage: cos ai budget <show|reset|history> <app>".to_string()),
    }
}

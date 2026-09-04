//! `cos::ai` — the App–AI Gate (internal kernel module).
//!
//! Every App-driven LLM / image / TTS / STT / vision call passes
//! through this subsystem. The gate enforces, in order:
//!
//!   1. Capability — the calling app's session must hold the AI verb
//!      (e.g. `ai.chat`) at a `name` scope covering the requested
//!      model.
//!   2. Manifest policy — the app's manifest must declare an `ai`
//!      block; the prompt's declared `origin` must be in its origin
//!      list. The kernel-owned model is then layered on top.
//!   3. User override — a per-app file under `$HOME/.config/cos/apps/`
//!      may **tighten** (never loosen) the manifest's budget, safety,
//!      or origin list, or kill-switch the app entirely.
//!   4. Consent — the user must have approved a snapshot of the
//!      manifest's AI block (`$HOME/.config/cos/consents/<id>.json`).
//!      Missing or drifted snapshots deny with `consent_required` /
//!      `consent_stale`.
//!   5. Budget — two axes, both denominated in tokens:
//!       * per-app cap from the manifest (`AiBudget::monthly_units`)
//!       * per-user aggregate cap at `$HOME/.config/cos/ai/budget.json`
//!         (opt-in: missing file or `monthly_units == 0` ⇒ no cap)
//!      A conservative input+maximum-output bound is reserved pre-call;
//!      settlement atomically releases that reservation and records actual
//!      usage. Cap checks include every in-flight reservation, and either
//!      ceiling tripping hard-denies the call.
//!   6. Safety — `Strict` redacts secrets in the prompt before
//!      sending it upstream. `Minimal` is audit-only.
//!   7. Audit — every accepted and denied call is logged.
//!
//! Apps **never** talk to providers directly. The frontend desktop
//! talks to apps; apps shell out to `cos ai chat --app <id>`; the
//! dispatcher in [`chat`] then calls into [`gate`] after every gate
//! above has cleared.
//!
//! ## What this module does NOT do
//!
//! - It does not host the per-turn agent loop. `cos agent ask` and
//!   the REPL form of `cos agent chat` keep their own kernel-only
//!   loops with full tool/memory wiring — those belong to the
//!   *kernel Agent product* ([`crate::agent`]), not to Apps.
//! - It does not classify or detect prompt injection beyond routing
//!   `external-content` through the strict safety profile. The
//!   injection detector and classifier remain Phase 8 work.
//!
//! ## CLI surface
//!
//! - `cos ai chat --app <id> …` — one-shot App-gated call. The only
//!   sanctioned entry point for installed Apps to reach a model.
//! - `cos agent budget …` — per-App budget ledger (lives under
//!   `cos agent` for historical reasons; will likely move under
//!   `cos ai budget` in a future cleanup).
//! - `cos app consent …` — per-App user consent, lives under
//!   `cos app` because it is a per-App user decision.
//!
//! `cos agent chat` and `cos agent ask` are *not* App entry points.
//! They are the kernel Agent's own surface and Apps must not invoke
//! them.

pub mod budget;
pub mod chat;
pub mod consent;
pub mod gate;
pub mod overrides;
pub mod tools;
pub mod user_budget;

pub(super) fn wire_error(
    code: &'static str,
    message: impl Into<String>,
    detail: Option<serde_json::Value>,
) -> String {
    let mut error = serde_json::json!({
        "error": message.into(),
        "code": code,
    });
    if let Some(detail) = detail.filter(serde_json::Value::is_object) {
        error["detail"] = detail;
    }
    error.to_string()
}

pub(super) fn invalid_args(message: impl Into<String>) -> String {
    wire_error("INVALID_ARGS", message, None)
}

pub(super) fn permission_denied(
    message: impl Into<String>,
    detail: Option<serde_json::Value>,
) -> String {
    wire_error("PERMISSION_DENIED", message, detail)
}

/// Dispatcher for `cos ai <command>`. Exposes:
///   * `chat` — single-shot, gated, modality-derived LLM call.
///   * `tool` — single Tool invocation from the App-facing catalog
///     (see [`tools::CATALOG`]).
///   * `tools` — print the catalog as JSON for App authors and LLM
///     function-call spec generation.
pub fn run(command: &str, args: &[String]) -> Result<serde_json::Value, String> {
    match command {
        "chat" => chat::chat_cmd(args),
        "tool" => tool_cmd(args),
        "tools" => tools_list_cmd(args),
        other => Err(wire_error(
            "UNKNOWN_VERB",
            format!("unknown command: cos ai {other}. try: chat | tool | tools"),
            None,
        )),
    }
}

/// Implements `cos ai tool <name> --app <id> [--args <json>|--args-file <p>]`.
///
/// Identity is enforced via the same helper `cos ai chat` uses:
/// env claim, registered `app_id`, and nearest App ancestry must agree.
fn tool_cmd(args: &[String]) -> Result<serde_json::Value, String> {
    let mut name: Option<String> = None;
    let mut app: Option<String> = None;
    let mut args_json: Option<String> = None;
    let mut args_file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => {
                app = args.get(i + 1).cloned();
                i += 2;
            }
            "--args" => {
                args_json = args.get(i + 1).cloned();
                i += 2;
            }
            "--args-file" => {
                args_file = args.get(i + 1).cloned();
                i += 2;
            }
            other if !other.starts_with("--") && name.is_none() => {
                name = Some(other.to_string());
                i += 1;
            }
            other => {
                return Err(invalid_args(format!(
                    "unknown flag for `cos ai tool`: {other}"
                )));
            }
        }
    }

    let name = name.ok_or_else(|| {
        invalid_args("missing tool name. usage: cos ai tool <name> --app <id> --args <json>")
    })?;
    let app = app.ok_or_else(|| invalid_args("--app is required"))?;

    chat::enforce_identity_for(&app).map_err(|error| permission_denied(error, None))?;

    let raw = match (args_json, args_file) {
        (Some(s), _) => s,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| invalid_args(format!("--args-file {path}: {e}")))?,
        (None, None) => "{}".to_string(),
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| invalid_args(format!("--args is not valid JSON: {e}")))?;

    let result = tools::execute(&name, &app, &parsed).map_err(|error| error.to_wire_error())?;
    serde_json::to_value(result)
        .map_err(|error| wire_error("INTERNAL_ERROR", error.to_string(), None))
}

/// Implements `cos ai tools` — print the App-facing Tool catalog.
fn tools_list_cmd(args: &[String]) -> Result<serde_json::Value, String> {
    if !args.is_empty() {
        return Err(invalid_args(format!(
            "`cos ai tools` takes no arguments; got: {}",
            args.join(" ")
        )));
    }
    let entries: Result<Vec<_>, String> = tools::CATALOG
        .iter()
        .map(|t| {
            let args_schema = serde_json::from_str::<serde_json::Value>(t.args_schema)
                .map_err(|error| wire_error("INTERNAL_ERROR", error.to_string(), None))?;
            let returns_schema = serde_json::from_str::<serde_json::Value>(t.returns_schema)
                .map_err(|error| wire_error("INTERNAL_ERROR", error.to_string(), None))?;
            Ok(serde_json::json!({
                "name": t.name,
                "summary": t.summary,
                "verb": t.verb.as_str(),
                "stability": match t.stability {
                    tools::Stability::Stable => "stable",
                    tools::Stability::Experimental => "experimental",
                },
                "args_schema": args_schema,
                "returns_schema": returns_schema,
            }))
        })
        .collect();
    Ok(serde_json::json!({ "tools": entries? }))
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/ai.rs"));
}

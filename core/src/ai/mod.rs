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
//!      Reserved pre-call and finalised after the provider returns;
//!      either ceiling tripping hard-denies the call.
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
        other => Err(format!(
            "unknown command: cos ai {other}. try: chat | tool | tools"
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
                return Err(format!("unknown flag for `cos ai tool`: {other}"));
            }
        }
    }

    let name = name.ok_or_else(|| {
        "missing tool name. usage: cos ai tool <name> --app <id> --args <json>".to_string()
    })?;
    let app = app.ok_or_else(|| "--app is required".to_string())?;

    chat::enforce_identity_for(&app)?;

    let raw = match (args_json, args_file) {
        (Some(s), _) => s,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| format!("--args-file {path}: {e}"))?,
        (None, None) => "{}".to_string(),
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("--args is not valid JSON: {e}"))?;

    let result = tools::execute(&name, &app, &parsed)?;
    Ok(serde_json::to_value(result).unwrap_or(serde_json::json!({})))
}

/// Implements `cos ai tools` — print the App-facing Tool catalog.
fn tools_list_cmd(args: &[String]) -> Result<serde_json::Value, String> {
    if !args.is_empty() {
        return Err(format!(
            "`cos ai tools` takes no arguments; got: {}",
            args.join(" ")
        ));
    }
    let entries: Vec<_> = tools::CATALOG
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "summary": t.summary,
                "verb": t.verb.as_str(),
                "stability": match t.stability {
                    tools::Stability::Stable => "stable",
                    tools::Stability::Experimental => "experimental",
                },
                "args_schema": serde_json::from_str::<serde_json::Value>(t.args_schema).unwrap_or(serde_json::json!({})),
                "returns_schema": serde_json::from_str::<serde_json::Value>(t.returns_schema).unwrap_or(serde_json::json!({})),
            })
        })
        .collect();
    Ok(serde_json::json!({ "tools": entries }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_routes_known_subcommands_and_rejects_unknown() {
        let err = run("frobnicate", &[]).unwrap_err();
        assert!(err.contains("unknown command"), "got: {err}");
        assert!(err.contains("chat"), "got: {err}");
        assert!(err.contains("tool"), "got: {err}");
    }

    #[test]
    fn tools_list_returns_catalog_as_json() {
        let v = tools_list_cmd(&[]).unwrap();
        let arr = v.get("tools").and_then(|x| x.as_array()).expect("tools array");
        assert!(!arr.is_empty(), "catalog should not be empty");
        for t in arr {
            assert!(t.get("name").and_then(|x| x.as_str()).is_some());
            assert!(t.get("verb").and_then(|x| x.as_str()).is_some());
        }
    }

    #[test]
    fn tools_list_rejects_extra_args() {
        let err = tools_list_cmd(&["unexpected".into()]).unwrap_err();
        assert!(err.contains("no arguments"), "got: {err}");
    }

    #[test]
    fn tool_cmd_requires_name() {
        let err = tool_cmd(&["--app".into(), "x".into()]).unwrap_err();
        assert!(err.contains("missing tool name"), "got: {err}");
    }

    #[test]
    fn tool_cmd_requires_app() {
        let err = tool_cmd(&["fs.read_text".into()]).unwrap_err();
        assert!(err.contains("--app"), "got: {err}");
    }
}

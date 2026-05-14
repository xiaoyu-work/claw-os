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
pub mod user_budget;

/// Dispatcher for `cos ai <command>`. Currently exposes only `chat`;
/// `tool` (single-Tool execution) and the App-facing Tool catalog
/// land in later phases (see `docs/app-ai-integration.md` §11).
pub fn run(command: &str, args: &[String]) -> Result<serde_json::Value, String> {
    match command {
        "chat" => chat::chat_cmd(args),
        other => Err(format!(
            "unknown command: cos ai {other}. try: chat"
        )),
    }
}


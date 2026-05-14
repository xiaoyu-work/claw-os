//! `cos::ai` — the App–AI Gate (internal kernel module).
//!
//! Every app-driven LLM / image / TTS / STT / vision call passes
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
//! talks to apps; apps shell out to `cos agent chat --app <id>`;
//! the agent dispatcher then calls into `ai::gate` after every gate
//! above has cleared.
//!
//! ## What this module does NOT do
//!
//! - It does not host the per-turn agent loop. `cos agent ask` / the
//!   REPL form of `cos agent chat` keep their own kernel-only loops
//!   with full tool/memory wiring.
//! - It does not classify or detect prompt injection beyond routing
//!   `external-content` through the strict safety profile. The
//!   injection detector and classifier remain Phase 8 work.
//!
//! ## CLI surface
//!
//! There is no `cos ai …` namespace. The user-facing CLI lives under
//! [`crate::agent`]: `cos agent chat --app <id> --prompt …` for the
//! one-shot app-gated call, and `cos agent budget …` for the per-app
//! budget ledger. Consent management lives under `cos app consent`
//! since it is a per-app user decision rather than an agent runtime
//! concern. The helpers below are kernel-internal.

pub mod budget;
pub mod consent;
pub mod gate;
pub mod overrides;
pub mod user_budget;

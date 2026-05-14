//! `cos::ai` — the App–AI Gate (internal kernel module).
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
//!   3. Budget — the per-app monthly token cap. Reserved
//!      pre-call and finalised after the provider returns; over-cap
//!      requests are hard-denied.
//!   4. Safety — `Strict` redacts secrets in the prompt before
//!      sending it upstream. `Minimal` is audit-only.
//!   5. Audit — every accepted and denied call is logged.
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
//! budget ledger. The helpers below are kernel-internal.

pub mod budget;
pub mod gate;

//! Agent runtime — main loop, turn orchestration, scheduler, hooks.
//!
//! Phase 1 lands the real `loop_` driver (Hermes `run_agent.py` reference).

pub mod hooks;
pub mod loop_;
pub mod turn;

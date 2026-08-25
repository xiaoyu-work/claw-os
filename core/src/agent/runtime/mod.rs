//! Agent runtime — main loop, turn orchestration, scheduler, hooks.

pub mod approval;
pub mod auto_curator;
pub mod background;
pub mod evidence;
pub mod hooks;
pub mod hooks_config;
pub mod interrupt;
pub mod loop_;
pub mod presentation;
pub mod progress;
pub mod semantic_indexer;
pub mod turn;

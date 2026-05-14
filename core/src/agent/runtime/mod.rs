//! Agent runtime — main loop, turn orchestration, scheduler, hooks.

pub mod approval;
pub mod auto_curator;
pub mod background;
pub mod hooks;
pub mod hooks_config;
pub mod interrupt;
pub mod loop_;
pub mod semantic_indexer;
pub mod turn;

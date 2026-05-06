//! Tool registry + exec proxies into cos primitives.
//!
//! Tools are thin Rust wrappers that call into existing cos modules
//! (fs/exec/net/web/proc/sandbox/checkpoint/etc.) instead of re-implementing
//! Hermes's parallel reimplementations. See plan.md section A for the mapping.

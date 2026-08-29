//! Task-owned host for dynamic App and MCP processes.
//!
//! `claw-extension-host` is deliberately separate from both the privileged
//! broker and the model/tool worker. The broker owns its lifetime and a
//! private, route-filtered proxy socket; `claw-agentd` reaches it through a
//! second socket that is bound to the exact worker pid/start-time and task
//! lease.

#[cfg(unix)]
pub mod broker;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod host;
pub mod protocol;
#[cfg(unix)]
pub mod spawn;

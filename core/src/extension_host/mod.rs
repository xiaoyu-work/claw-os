//! Isolated host for dynamic App and MCP processes.
//!
//! `claw-extension-host` is deliberately separate from both the privileged
//! broker and the model/tool worker. The broker owns its lifetime and a
//! private, route-filtered proxy socket. Legacy task Hosts bind their control
//! socket to one worker pid/start-time; persistent MCP App Hosts bind it to
//! the root daemon and serve one authenticated owner across Agent tasks.

#[cfg(unix)]
pub mod broker;
pub(crate) mod child_isolation;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod host;
#[cfg(unix)]
pub mod identity;
pub mod protocol;
#[cfg(unix)]
pub mod spawn;

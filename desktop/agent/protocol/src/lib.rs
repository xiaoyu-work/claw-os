//! Versioned HTTP and SSE presentation contract for the desktop Agent.
//!
//! This crate deliberately knows nothing about the UI, bridge implementation,
//! or clawd/core models. The bridge translates those lower-level models into
//! this contract before data crosses the loopback HTTP boundary.

mod activities;
mod automatic_work;
mod capability_policy;
mod continuity;
mod execution_limits;
mod http;
mod followups;
mod memory;
mod monetary_budget;
mod object_state;
mod presentation;
mod scheduling_priority;
mod stream;
mod version;

pub use activities::*;
pub use automatic_work::*;
pub use capability_policy::*;
pub use continuity::*;
pub use execution_limits::*;
pub use http::*;
pub use followups::*;
pub use memory::*;
pub use monetary_budget::*;
pub use object_state::*;
pub use presentation::*;
pub use scheduling_priority::*;
pub use stream::*;
pub use version::*;
pub use clawd_client::nearby;

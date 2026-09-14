//! Versioned HTTP and SSE presentation contract for the desktop Agent.
//!
//! This crate deliberately knows nothing about the UI, bridge implementation,
//! or clawd/core models. The bridge translates those lower-level models into
//! this contract before data crosses the loopback HTTP boundary.

mod activities;
mod capability_policy;
mod execution_limits;
mod http;
mod monetary_budget;
mod object_state;
mod stream;
mod version;

pub use activities::*;
pub use capability_policy::*;
pub use execution_limits::*;
pub use http::*;
pub use monetary_budget::*;
pub use object_state::*;
pub use stream::*;
pub use version::*;

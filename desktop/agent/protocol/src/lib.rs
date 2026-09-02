//! Versioned HTTP and SSE presentation contract for the desktop Agent.
//!
//! This crate deliberately knows nothing about the UI, bridge implementation,
//! or clawd/core models. The bridge translates those lower-level models into
//! this contract before data crosses the loopback HTTP boundary.

mod http;
mod stream;
mod version;

pub use http::*;
pub use stream::*;
pub use version::*;

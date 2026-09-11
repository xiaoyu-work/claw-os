//! Root-owned login/display activation and GUI instance custody.

pub mod client;
mod containment;
pub(crate) use containment::protected_executable;
pub(crate) mod creator;
pub mod host;
pub mod registry;
pub(crate) mod runtime;

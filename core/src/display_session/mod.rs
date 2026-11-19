//! Root-owned login/display activation and GUI instance custody.

pub mod client;
mod containment;
pub(crate) use containment::protected_executable;
pub(crate) mod creator;
pub mod host;
mod locker;
pub mod registry;
pub(crate) mod runtime;

pub(crate) const SHELL_APP: &str = "desktop-shell";

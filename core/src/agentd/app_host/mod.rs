//! Controlled App hosting inside the existing unprivileged task worker.
//!
//! The job channel admits only the closed control methods below. App execution
//! stays in the ordinary sandbox; the broker remains the authority.

#[cfg(unix)]
pub mod broker;
pub mod protocol;

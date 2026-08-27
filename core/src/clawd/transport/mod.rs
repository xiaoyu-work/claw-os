//! Broker socket transport: framing, per-message peer credentials and
//! admission control.
//!
//! `server.rs` owns the daemon's lifecycle and dispatch; everything
//! about *how a message arrives and who sent it* lives here, so the two
//! concerns can be reasoned about — and tested — apart.

pub mod frame;
pub mod limits;
pub mod peer;

pub use frame::{PeerStream, ReadOutcome, RequestFrame};
pub use limits::{mutation_key, Admission, Limits};
pub use peer::{Credentials, PeerProcess};

//! OS-private display activation and compositor control, not an App SDK.

#![cfg(target_os = "linux")]

pub mod compositor;
pub mod identity;
pub mod install;
pub mod listener;
pub mod locale;
pub mod session;
pub mod transport;
pub mod wire;
pub mod workload;

pub use identity::{KernelLogin, ProcessIdentity};
pub use listener::Listener;
pub use transport::{Connection, Error, Packet, Received};
pub use wire::{CompositorCommand, CompositorReply, Epoch, InstanceId};

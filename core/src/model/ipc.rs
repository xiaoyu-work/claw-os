//! IPC between `cos model infer` (client) and the model-runtime daemon.
//!
//! Uses the existing `crate::ipc` primitives over a Unix socket at
//! `paths::socket_path()` (`/run/cos/model-runtime.sock` by default).

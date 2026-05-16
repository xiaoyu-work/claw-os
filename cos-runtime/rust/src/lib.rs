//! # cos-runtime
//!
//! **Internal** runtime for the claw-os bundled apps and desktop GUI
//! binaries. Not a developer SDK — third-party Linux apps written for
//! claw-os should not pull this crate in. Use
//! [`claw-os-sdk`](https://docs.rs/claw-os-sdk) for AI / tool / agent
//! integration.
//!
//! ## What's in here
//!
//! Every gated `cos app <id> <verb>` call from the apps in `apps/*`
//! and the cosmic-desktop binaries under `desktop/*` is routed through
//! one of these modules:
//!
//! | Module        | Wire family | Equivalent CLI             |
//! |---------------|-------------|----------------------------|
//! | [`policy`]    | `perms`     | `cos perms check / grant`  |
//! | [`fs`]        | `app`       | `cos app fs ...`           |
//! | [`exec`]      | `app`       | `cos app exec ...`         |
//! | [`pkg`]       | `app`       | `cos app pkg ...`          |
//! | [`notify`]    | `app`       | `cos app notify ...`       |
//! | [`net`]       | `app`       | `cos app net ...`          |
//!
//! Apart from `policy`, every module is a thin typed wrapper around
//! `cos app <id> <verb>` and exists so that capability gating, audit,
//! and session checkpointing happen uniformly regardless of whether a
//! mutation originated from the terminal, a Python app, or a cosmic
//! desktop binary.
//!
//! ## Wire layer
//!
//! Transport (`call`, `call_typed`, `cos_call_json`) lives in
//! `claw-os-sdk` and is re-exported here so the modules' internal
//! `super::*` continues to resolve. cos-runtime adds no new transport
//! semantics on top of the public SDK.

pub use claw_os_sdk::{call, call_typed, cos_call_json, BridgeError, Error};

pub mod exec;
pub mod fs;
pub mod net;
pub mod notify;
pub mod pkg;
pub mod policy;

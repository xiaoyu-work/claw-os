//! Model Context Protocol (MCP) — minimal in-tree implementation.
//!
//! The plan calls out [`rmcp`](https://crates.io/crates/rmcp) for
//! production use but pulling that into the kernel binary is a
//! significant blast radius (tokio + tower + tracing-subscriber etc.
//! all transitively). The MCP wire protocol is JSON-RPC 2.0 over a
//! framed stream (typically stdio) with a small, stable method set;
//! we ship a hand-rolled scaffold so the kernel can talk MCP today
//! and adopt rmcp later without changing call sites.
//!
//! Submodules:
//! * [`protocol`] — request / response / notification types and the
//!   JSON-RPC envelope.
//! * [`client`] — drives an outbound MCP connection (we are the
//!   *client*; the remote is a server like a database adapter).
//! * [`server`] — accepts inbound MCP requests (we are the *server*;
//!   external agents call us as a tool catalogue / resource provider).
//! * [`transport`] — `Transport` trait + an in-memory pair used by
//!   tests. Stdio / TCP / Unix-socket transports plug in here.
//! * `oauth` (Phase 6.x) — token storage + refresh against
//!   `crate::credential`. Out of scope for this scaffold.

pub mod client;
pub mod discover;
pub mod integration;
pub mod protocol;
pub mod server;
pub mod transport;

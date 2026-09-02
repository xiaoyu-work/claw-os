//! Embeddable MCP (Model Context Protocol) stdio server for Claw OS
//! native Apps.
//!
//! This module is the Rust counterpart to `claw_os_sdk.mcp`. It lets
//! any Rust binary (typically a desktop GUI App) opt into a second
//! mode in which it speaks MCP JSON-RPC over stdio so the kernel
//! agent can invoke its tools.
//!
//! ## Usage shape
//!
//! ```ignore
//! use claw_os_sdk::mcp::{Server, Tool, ToolResult};
//! use async_trait::async_trait;
//! use std::sync::Arc;
//!
//! struct EchoTool;
//!
//! #[async_trait]
//! impl Tool for EchoTool {
//!     fn name(&self) -> &'static str { "echo" }
//!     fn description(&self) -> &'static str { "Echo the `text` arg back." }
//!     fn input_schema(&self) -> serde_json::Value {
//!         serde_json::json!({
//!             "type": "object",
//!             "properties": {"text": {"type": "string"}},
//!             "required": ["text"],
//!             "additionalProperties": false,
//!         })
//!     }
//!     async fn exec(&self, input: serde_json::Value) -> ToolResult {
//!         let t = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
//!         ToolResult::ok(t.to_string())
//!     }
//! }
//!
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() {
//!     // Typical App pattern: switch on the env var the kernel sets.
//!     if std::env::var("COS_MCP_SERVER").as_deref() != Ok("1") {
//!         // ...run the GUI / normal mode here...
//!         return;
//!     }
//!     Server::new("my-app", env!("CARGO_PKG_VERSION"))
//!         .tool(Arc::new(EchoTool))
//!         .serve_stdio()
//!         .await
//!         .expect("MCP server failed");
//! }
//! ```
//!
//! ## Scope
//!
//! Server-only. Clients live in the kernel (`core/src/agent/tools/mcp/
//! client.rs`). Keeping this implementation in the public SDK lets
//! native Apps expose MCP tools without depending on kernel or
//! `cos-runtime` internals.

mod generated;
pub mod protocol;
pub mod server;
pub mod tool;
pub mod transport;

pub use server::{Server, ServerError};
pub use tool::{Tool, ToolResult};
pub use transport::{StdioTransport, Transport, TransportError};

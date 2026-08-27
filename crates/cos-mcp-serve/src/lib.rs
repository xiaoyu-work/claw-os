//! Embeddable MCP (Model Context Protocol) stdio server for Claw OS
//! native Apps.
//!
//! This crate is the Rust counterpart to `claw-os-sdk/python/src/claw_os_sdk/serve.py`. It lets
//! any Rust binary (typically a libcosmic GUI App) opt into a second
//! mode in which it speaks MCP JSON-RPC over stdio so the kernel
//! agent can invoke its tools.
//!
//! ## Usage shape
//!
//! ```ignore
//! use cos_mcp_serve::{Server, Tool, ToolResult};
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
//! client.rs`). The wire types are duplicated between the two for
//! now to keep desktop binaries off the kernel's dependency surface;
//! a future cleanup can extract a shared `cos-mcp-types` crate.

mod generated;
pub mod protocol;
pub mod server;
pub mod tool;
pub mod transport;

pub use server::{Server, ServerError};
pub use tool::{Tool, ToolResult};
pub use transport::{StdioTransport, Transport, TransportError};

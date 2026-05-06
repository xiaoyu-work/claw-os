//! `cos agent` — agent-native OS subsystem.
//!
//! Phase 0: skeleton + subcommand dispatch only. Real loop wired in Phase 1.
//!
//! Module layout (target architecture):
//!
//! ```text
//! agent/
//! ├── mod.rs          (this file: subcommand dispatcher)
//! ├── runtime/        loop_, scheduler, turn, hooks
//! ├── prompt/         system prompt, MEMORY.md, USER.md injection
//! ├── context/        session, history, compression
//! ├── memory/         sqlite_fts, semantic, honcho, curator
//! ├── skills/         skill registry, loader, exec
//! ├── llm/            Provider trait + provider impls (anthropic, openai, ...)
//! ├── tools/          tool registry, exec proxies into cos primitives
//! └── safety/         redact, policy hooks, approval
//! ```

pub mod context;
pub mod llm;
pub mod memory;
pub mod prompt;
pub mod runtime;
pub mod safety;
pub mod skills;
pub mod tools;

use serde_json::{json, Value};

/// Dispatch a `cos agent <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "ask" => {
            let prompt = args.first().cloned().unwrap_or_default();
            if prompt.is_empty() {
                return Err("usage: cos agent ask \"<prompt>\"".into());
            }
            // Phase 0: stub. Phase 1 wires runtime::loop_::ask().
            Ok(json!({
                "status": "not_implemented",
                "phase": "0",
                "message": "agent runtime arrives in Phase 1",
                "received_prompt": prompt,
            }))
        }
        "chat" => Ok(json!({"status": "not_implemented", "phase": "1"})),
        "status" => Ok(json!({
            "status": "ok",
            "phase": "0-skeleton",
            "providers": llm::available_providers(),
            "tools_registered": 0,
            "skills_loaded": 0,
        })),
        "service" => Ok(json!({"status": "not_implemented", "phase": "1+"})),
        other => Err(format!(
            "unknown command: {other}. try: ask | chat | status | service"
        )),
    }
}

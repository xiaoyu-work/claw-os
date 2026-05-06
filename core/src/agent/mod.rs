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
pub mod display;
pub mod insights;
pub mod llm;
pub mod media;
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
            match runtime::loop_::ask_blocking(&prompt) {
                Ok(result) => Ok(json!({
                    "answer": result.answer,
                    "turns": result.turns,
                    "provider": result.provider,
                    "model": result.model,
                    "session_id": result.session_id,
                })),
                Err(e) => Err(e.to_string()),
            }
        }
        "chat" => Ok(json!({"status": "not_implemented", "phase": "1+"})),
        "status" => {
            let cfg = &crate::config::get().agent;
            let tools = tools::registry::default_registry();
            // Best-effort memory DB stats — read-only, never mutates.
            let memory_stats = match memory::sqlite_fts::MemoryDb::open_default() {
                Ok(db) => {
                    let total = db.count_total().unwrap_or(0);
                    let sessions = db.sessions(1).map(|s| s.len()).unwrap_or(0);
                    json!({
                        "status": "ok",
                        "path": crate::paths::agent_memory_db_path().display().to_string(),
                        "total_messages": total,
                        "has_sessions": sessions > 0,
                    })
                }
                Err(e) => json!({ "status": "unavailable", "error": e.to_string() }),
            };
            Ok(json!({
                "status": "ok",
                "phase": "3",
                "provider": cfg.provider,
                "provider_registered": llm::registry::is_registered(&cfg.provider),
                "providers": llm::available_providers(),
                "model": cfg.model,
                "max_turns": cfg.max_turns,
                "max_tokens": cfg.max_tokens,
                "temperature": cfg.temperature,
                "tools_registered": tools.len(),
                "tools": tools.names(),
                "skills_loaded": 0,
                "memory": memory_stats,
            }))
        }
        "service" => Ok(json!({"status": "not_implemented", "phase": "1+"})),
        other => Err(format!(
            "unknown command: {other}. try: ask | chat | status | service"
        )),
    }
}

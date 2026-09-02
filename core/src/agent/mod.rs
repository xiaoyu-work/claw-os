//! `cos agent` — agent-native OS subsystem.
//!
//! Command implementations live in responsibility-specific sibling modules.
//! This module owns only subsystem composition and top-level CLI routing.

pub mod audit_cli;
pub mod classify;
pub mod context;
pub mod curator;
pub mod curator_author;
pub mod curator_drafts;
pub mod diagnose;
pub mod display;
pub mod doctor_cli;
pub mod insights;
pub mod lifecycle;
pub mod llm;
pub mod media;
pub mod memory;
pub mod nudge;
pub mod prompt;
pub mod replay_cli;
pub mod run_log_cli;
pub mod runtime;
pub mod safety;
pub mod service;
pub mod setup;
pub mod shell_hooks;
pub mod skills;
pub mod summarise;
pub mod title;
pub mod tools;
pub mod trust;
pub mod util;
pub mod web;

mod app_ai_commands;
mod command_catalog;
mod conversation_commands;
mod curator_commands;
mod developer_commands;
mod diagnostic_commands;
mod mcp_commands;
mod media_commands;
mod memory_commands;
mod model_commands;
mod provider_commands;
mod safety_commands;
mod session_commands;
mod skills_commands;
mod task_commands;
mod text_commands;
mod vision_commands;

use serde_json::Value;

use crate::clawd::agent_client;

pub(crate) use command_catalog::dev_help;
pub(crate) use diagnostic_commands::{
    usage_cmd_from_reader, usage_for_current_context, usage_primitive,
};
use provider_commands::{provider_doctor_cmd, run_active_provider_probe};

/// Dispatch a `cos agent <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "ask" => conversation_commands::ask_cmd(args),
        "chat" => conversation_commands::chat_cmd(args),
        "serve" => web::serve(args),
        "budget" => app_ai_commands::budget_cmd(args),
        "override" => app_ai_commands::override_cmd(args),
        "usage" => diagnostic_commands::usage_public_cmd(args),
        "status" => provider_commands::status_cmd(),
        "service" => agent_client::service_cmd(args),
        "recall" => session_commands::recall_cmd(args),
        "sessions" => session_commands::sessions_cmd(args),
        "ls" => lifecycle::ls(args),
        "show" => lifecycle::show(args),
        "stop" => lifecycle::stop(args),
        "undo" => lifecycle::undo(args),
        "resume" => lifecycle::resume(args),
        "setup" => setup::run(args),
        "notes" => memory_commands::notes_cmd(args),
        "memory" => memory_commands::memory_cmd(args),
        "skills" => skills_commands::skills_cmd(args),
        "mcp" => mcp_commands::mcp_cmd(args),
        "todo" => task_commands::todo_cmd(args),
        "doctor" => doctor_cli::doctor_cmd(args),
        "diagnose" => diagnose::diagnose_cmd(args),
        "dev" => dev_dispatch(args),
        other => Err(format!(
            "unknown command: {other}. try: setup | ask | chat | serve | budget | override | usage | status | sessions | recall | service | notes | memory | skills | todo | mcp | doctor | diagnose | dev | ls | show | stop | undo | resume"
        )),
    }
}

/// `cos agent dev <subcmd>` — internal / power-user namespace.
fn dev_dispatch(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match sub {
        "" | "list" | "--help" | "-h" => Ok(command_catalog::dev_help()),
        "insights" => diagnostic_commands::insights_cmd(&rest),
        "usage" => diagnostic_commands::usage_for_current_context(&rest),
        "audit" => audit_cli::audit_cmd(&rest),
        "replay" => replay_cli::replay_cmd(&rest),
        "run-log" | "run_log" => run_log_cli::run_log_cmd(&rest),
        "providers" => provider_commands::providers_cmd(&rest),
        "provider-doctor" => provider_commands::provider_doctor_cmd(&rest),
        "llm" => model_commands::llm_cmd(&rest),
        "prompt" => text_commands::prompt_cmd(&rest),
        "tools" => safety_commands::tools_cmd(&rest),
        "guardrails" => safety_commands::guardrails_cmd(&rest),
        "approval" => safety_commands::approval_cmd(&rest),
        "redact" => safety_commands::redact_cmd(&rest),
        "think-scrub" => text_commands::think_scrub_cmd(&rest),
        "tokens" => text_commands::tokens_cmd(&rest),
        "title" => text_commands::title_cmd(&rest),
        "summarise" | "summarize" => text_commands::summarise_cmd(&rest),
        "classify" => text_commands::classify_cmd(&rest),
        "display" => diagnostic_commands::display_cmd(&rest),
        "binary-ext" => safety_commands::binary_ext_cmd(&rest),
        "file-safety" => safety_commands::file_safety_cmd(&rest),
        "context" => developer_commands::context_cmd(&rest),
        "compress" => model_commands::compress_cmd(&rest),
        "aux" | "auxiliary" => model_commands::aux_cmd(&rest),
        "retry" => model_commands::retry_cmd(&rest),
        "vision" => vision_commands::vision_cmd(&rest),
        "osv" => safety_commands::osv_cmd(&rest),
        "curator" => curator_commands::curator_cmd(&rest),
        "nudge" => task_commands::nudge_cmd(&rest),
        "shell-hooks" => diagnostic_commands::shell_hooks_cmd(&rest),
        "media" => media_commands::media_cmd(&rest),
        "semantic" => memory_commands::semantic_cmd(&rest),
        "interrupt" => developer_commands::interrupt_cmd(&rest),
        "learn" => memory_commands::learn_cmd(&rest),
        "hooks" => developer_commands::hooks_cmd(&rest),
        other => Err(format!(
            "unknown dev subcommand: {other}. run `cos agent dev` for the list."
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/mod.rs"
    ));
}

//! `cos agent honcho` — operate the Honcho dialectic memory client
//! from the shell.
//!
//! Honcho is opt-in (see [`crate::agent::memory::honcho`] for details).
//! When `HONCHO_BASE_URL` is unset, every subcommand reports
//! `configured: false` and refuses to make a request.
//!
//! Subcommands:
//!
//! - `status` — show whether Honcho is configured (env vars) and
//!   echo the resolved base URL / workspace / timeout. Never makes
//!   a network request.
//! - `append --session SID --peer PID --content TEXT [--role user|assistant]`
//!   — record one message into a session.
//! - `query --peer PID --query TEXT [--session SID]` — run a
//!   dialectic query and print the engine's response.
//!
//! All subcommands return JSON to stdout. Network errors are surfaced
//! as `Err(..)` so the dispatcher can render them with a non-zero
//! exit. This is the only Honcho consumer in the codebase that
//! propagates errors to the user; the runtime integration (next
//! commit) will instead log warnings and continue.

use serde_json::{json, Value};

use crate::agent::memory::honcho::{HonchoClient, HonchoConfig, HonchoError, MessageRole};

/// Top-level dispatcher for `cos agent honcho <subcmd>`.
pub fn honcho_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "status" => cmd_status(),
        "append" => cmd_append(rest),
        "query" => cmd_query(rest),
        other => Err(format!(
            "unknown honcho subcommand: {other}. try: status | append | query"
        )),
    }
}

fn cmd_status() -> Result<Value, String> {
    match HonchoConfig::from_env() {
        None => Ok(json!({
            "configured": false,
            "reason": "HONCHO_BASE_URL not set",
        })),
        Some(cfg) => Ok(json!({
            "configured": true,
            "base_url": cfg.base_url,
            "workspace_id": cfg.workspace_id,
            "timeout_secs": cfg.timeout_secs,
            "auth": if cfg.api_key.is_some() { "bearer" } else { "none" },
        })),
    }
}

fn cmd_append(args: &[String]) -> Result<Value, String> {
    let session = require_string(args, "--session")?;
    let peer = require_string(args, "--peer")?;
    let content = require_string(args, "--content")?;
    let role = match parse_string_opt(args, "--role").as_deref() {
        Some("user") | None => MessageRole::User,
        Some("assistant") => MessageRole::Assistant,
        Some(other) => return Err(format!("invalid --role: {other} (expected user|assistant)")),
    };
    let cfg = require_config()?;
    let client = HonchoClient::new(cfg).map_err(|e| e.to_string())?;
    let url = client.messages_url(&session);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(client.append_message(&session, &peer, &content, role))
        .map_err(render_err)?;
    Ok(json!({
        "ok": true,
        "url": url,
        "session_id": session,
        "peer_id": peer,
        "role": match role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        },
    }))
}

fn cmd_query(args: &[String]) -> Result<Value, String> {
    let peer = require_string(args, "--peer")?;
    let query = require_string(args, "--query")?;
    let session = parse_string_opt(args, "--session");
    let cfg = require_config()?;
    let client = HonchoClient::new(cfg).map_err(|e| e.to_string())?;
    let url = client.chat_url(&peer);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let answer = rt
        .block_on(client.dialectic_query(&peer, &query, session.as_deref()))
        .map_err(render_err)?;
    Ok(json!({
        "ok": true,
        "url": url,
        "peer_id": peer,
        "session_id": session,
        "content": answer,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_config() -> Result<HonchoConfig, String> {
    HonchoConfig::from_env().ok_or_else(|| {
        "honcho is not configured. set HONCHO_BASE_URL (and optionally \
        HONCHO_API_KEY / HONCHO_WORKSPACE_ID / HONCHO_TIMEOUT_SECS), then \
        retry."
            .to_string()
    })
}

fn require_string(args: &[String], flag: &str) -> Result<String, String> {
    parse_string_opt(args, flag).ok_or_else(|| format!("missing required flag: {flag}"))
}

fn parse_string_opt(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            return iter.next().cloned();
        }
    }
    None
}

fn render_err(e: HonchoError) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    fn snapshot_and_clear_env() -> Vec<(String, Option<String>)> {
        let keys = [
            "HONCHO_BASE_URL",
            "HONCHO_API_KEY",
            "HONCHO_WORKSPACE_ID",
            "HONCHO_TIMEOUT_SECS",
        ];
        let saved: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, _) in &saved {
            std::env::remove_var(k);
        }
        saved
    }

    fn restore_env(saved: Vec<(String, Option<String>)>) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
    }

    #[test]
    fn unknown_subcommand_errors() {
        let err = honcho_cmd(&args(&["frobnicate"])).unwrap_err();
        assert!(err.contains("unknown honcho subcommand"), "got {err}");
    }

    #[test]
    fn status_reports_unconfigured_when_no_base_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        let v = honcho_cmd(&args(&["status"])).unwrap();
        restore_env(saved);
        assert_eq!(v["configured"], json!(false));
        assert!(v["reason"]
            .as_str()
            .unwrap_or("")
            .contains("HONCHO_BASE_URL"));
    }

    #[test]
    fn status_reports_configured_with_resolved_fields() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        std::env::set_var("HONCHO_BASE_URL", "https://h.example.com/v1/");
        std::env::set_var("HONCHO_API_KEY", "tok");
        std::env::set_var("HONCHO_WORKSPACE_ID", "ws-x");
        std::env::set_var("HONCHO_TIMEOUT_SECS", "5");
        let v = honcho_cmd(&args(&["status"])).unwrap();
        restore_env(saved);
        assert_eq!(v["configured"], json!(true));
        assert_eq!(v["base_url"], json!("https://h.example.com/v1"));
        assert_eq!(v["workspace_id"], json!("ws-x"));
        assert_eq!(v["timeout_secs"], json!(5));
        assert_eq!(v["auth"], json!("bearer"));
    }

    #[test]
    fn status_reports_no_auth_when_api_key_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        std::env::set_var("HONCHO_BASE_URL", "http://localhost:8000");
        let v = honcho_cmd(&args(&["status"])).unwrap();
        restore_env(saved);
        assert_eq!(v["configured"], json!(true));
        assert_eq!(v["auth"], json!("none"));
    }

    #[test]
    fn append_requires_configuration() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        let err = honcho_cmd(&args(&[
            "append",
            "--session",
            "s",
            "--peer",
            "p",
            "--content",
            "x",
        ]))
        .unwrap_err();
        restore_env(saved);
        assert!(err.contains("HONCHO_BASE_URL"), "got {err}");
    }

    #[test]
    fn append_requires_all_flags() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        std::env::set_var("HONCHO_BASE_URL", "http://localhost:9");
        // missing --content
        let err = honcho_cmd(&args(&["append", "--session", "s", "--peer", "p"])).unwrap_err();
        restore_env(saved);
        assert!(err.contains("--content"), "got {err}");
    }

    #[test]
    fn append_invalid_role_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        std::env::set_var("HONCHO_BASE_URL", "http://localhost:9");
        let err = honcho_cmd(&args(&[
            "append",
            "--session",
            "s",
            "--peer",
            "p",
            "--content",
            "x",
            "--role",
            "system",
        ]))
        .unwrap_err();
        restore_env(saved);
        assert!(err.contains("invalid --role"), "got {err}");
    }

    #[test]
    fn query_requires_peer_and_query() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        std::env::set_var("HONCHO_BASE_URL", "http://localhost:9");
        let err = honcho_cmd(&args(&["query", "--peer", "p"])).unwrap_err();
        restore_env(saved);
        assert!(err.contains("--query"), "got {err}");
    }
}

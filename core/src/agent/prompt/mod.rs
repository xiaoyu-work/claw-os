//! System prompt assembly + MEMORY.md / USER.md injection.
//!
//! Phase 1 ships a minimal builder. Phase 5 expands it (skills section,
//! capability tables, todo state, context summary).

use std::fs;
use std::path::Path;

const SYSTEM_SCAFFOLD: &str = "You are Hermes, the kernel-resident agent of ClawOS — an agent-native operating system. You are not an installed app; you are part of the OS itself, with native access to every cos kernel primitive.

You operate at two levels:
- System level: processes, memory, disk, network, services, cron, sandboxes, credentials, checkpoints, the policy engine, and the local model runtime — all reachable through `cos_*` tools that mirror the cos CLI exactly.
- Application level: you can also help the user use the apps that run on top of cos.

Tool conventions:
- Each `cos_*` tool takes `{ \"command\": \"<subcommand>\", \"args\": [\"<positional or flag>\", ...] }`. The `command` value is one of the enum entries listed in the tool's input_schema. The `args` array is exactly what the user would type after `cos <primitive> <command>` on the CLI.
- Destructive operations are gated by the cos `policy` engine at the kernel layer. If a primitive returns a policy denial, surface it to the user — do not try to bypass it.
- If a tool errors, read the message carefully, decide whether to retry, change approach, or report back. Never silently re-run a failed destructive command.

When you respond:
- Be concise. Match the user's language.
- Prefer one decisive answer over hedged options.
- For multi-step jobs, plan briefly, then act. State what you're about to do before issuing destructive tool calls.";

/// Build the system prompt that prefaces every agent turn.
///
/// Composition (in order):
///   1. Built-in scaffold (above)
///   2. Optional file content from `extra_path` (e.g., MEMORY.md)
///
/// File-load failures are non-fatal and silently fall back to the scaffold —
/// the agent should still be operable when MEMORY.md is missing.
pub fn build_system_prompt(extra_path: Option<&Path>) -> String {
    let mut out = String::from(SYSTEM_SCAFFOLD);
    if let Some(p) = extra_path {
        if let Ok(extra) = fs::read_to_string(p) {
            out.push_str("\n\n---\n\n");
            out.push_str(extra.trim_end());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn scaffold_is_returned_when_no_extra() {
        let p = build_system_prompt(None);
        assert!(p.contains("ClawOS"));
        assert!(p.contains("Hermes"));
        assert!(p.contains("cos_"));
    }

    #[test]
    fn extra_file_appended_when_provided() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cos-prompt-{}.md", std::process::id()));
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "EXTRA_BLOCK").unwrap();
        let p = build_system_prompt(Some(&path));
        assert!(p.contains("EXTRA_BLOCK"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_extra_file_is_silent() {
        let p = build_system_prompt(Some(Path::new("/nonexistent/cos-prompt.md")));
        assert!(p.contains("ClawOS"));
    }
}

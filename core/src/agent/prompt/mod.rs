//! System prompt assembly + MEMORY.md / USER.md / due-nudge injection.
//!
//! Composition (in order):
//!   1. Built-in scaffold — defines the agent's role and tool conventions.
//!   2. Auto-injected `MEMORY.md` and `USER.md` from
//!      [`crate::agent::memory::notes::NotesStore::system_default`]. Both
//!      files are read every time so updates by the model (via
//!      `cos_memory`) take effect on the next turn.
//!   3. Auto-injected due reminders from the periodic-nudge store at
//!      [`crate::paths::agent_nudges_path`]. Surfaces as a
//!      `<DUE_NUDGES>` block when at least one nudge is due (epoch
//!      seconds <= now). Empty-store / missing-file is silent.
//!   4. Optional explicit file from `extra_path` (overrides via
//!      `AgentConfig::system_prompt_path`).
//!
//! All injection is best-effort: missing or unreadable files are silently
//! skipped. The agent must remain operable when nothing exists.

pub mod caching;

use std::fs;
use std::path::Path;

use crate::agent::memory::notes::NotesStore;
use crate::agent::nudge::{now_epoch_s, NudgeStore};

const SYSTEM_SCAFFOLD: &str = "You are Claw, the kernel-resident agent of ClawOS — an agent-native operating system. You are not an installed app; you are part of the OS itself, with native access to every cos kernel primitive.

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
///   2. `MEMORY.md` and `USER.md` from the system notes store (auto-loaded)
///   3. Optional file content from `extra_path`
///
/// File-load failures are non-fatal and silently fall back to the scaffold —
/// the agent should still be operable when MEMORY.md is missing.
pub fn build_system_prompt(extra_path: Option<&Path>) -> String {
    let mut out = String::from(SYSTEM_SCAFFOLD);

    // Auto-injected notes (MEMORY.md, USER.md).
    if let Some(notes) = NotesStore::system_default().assemble_for_prompt() {
        out.push_str("\n\n---\n\n");
        out.push_str(&notes);
    }

    // Auto-injected due nudges. Reads `data_dir/agent/nudges.json`
    // every turn so newly-fired nudges drop out of the prompt as
    // soon as `nudge fire` updates them. NudgeStore swallows IO
    // errors via `Vec::new`, so missing/empty/corrupt files are silent.
    let store = NudgeStore::new(crate::paths::agent_nudges_path());
    let due = store.due(now_epoch_s());
    if !due.is_empty() {
        out.push_str("\n\n---\n\n<DUE_NUDGES>\n");
        for n in &due {
            out.push_str(&format!("- [{}] {}\n", n.id, n.message));
        }
        out.push_str("</DUE_NUDGES>");
    }

    // Explicit override file (e.g., per-session preface).
    if let Some(p) = extra_path {
        if let Ok(extra) = fs::read_to_string(p) {
            let trimmed = extra.trim_end();
            if !trimmed.is_empty() {
                out.push_str("\n\n---\n\n");
                out.push_str(trimmed);
            }
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
        assert!(p.contains("You are Claw,"));
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

    #[test]
    fn no_due_nudges_means_no_due_block() {
        // Without writing any nudges to the data dir, the
        // DUE_NUDGES block must be absent. (NudgeStore returns
        // Vec::new() for missing or unparseable files.)
        let p = build_system_prompt(None);
        assert!(!p.contains("<DUE_NUDGES>"));
    }
}

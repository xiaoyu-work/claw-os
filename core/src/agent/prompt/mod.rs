//! System prompt assembly + MEMORY.md / USER.md injection.
//!
//! Phase 1 ships a minimal builder. Phase 5 expands it (skills section,
//! capability tables, todo state, context summary).

use std::fs;
use std::path::Path;

const SYSTEM_SCAFFOLD: &str = "You are an OS-native agent running inside ClawOS, an agent-native operating system.

You can call tools to operate on the user's system. Use tools when the request needs information you don't have or when an action must be taken outside this conversation. Otherwise, answer directly.

When you respond:
- Be concise. Match the user's language.
- Prefer one decisive answer over hedged options.
- If a tool errors, explain the error and decide whether to retry, try a different approach, or report back to the user.";

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
        assert!(p.contains("tools"));
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

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

/// A single chunk of content that was auto-injected into the system
/// prompt at build time. Callers that own a `MemoryDb` handle should
/// persist each segment as an `injected` row so a later transcript
/// review can reconstruct exactly what the model saw, satisfying the
/// "model-visible means logged" invariant (issue #2, point 1).
///
/// `source` is a short stable tag (`memory_notes`, `due_nudges`,
/// `prompt_extra`) — the reader uses it to correlate a row with the
/// build-time origin. `content` is the raw text that was concatenated
/// into the prompt (i.e. what the model actually sees, *not* a summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedSegment {
    pub source: &'static str,
    pub content: String,
}

/// Stable source tags for [`InjectedSegment`]. Kept as `&'static str`
/// constants so the log rows can be filtered by exact string match
/// without a separate enum surface leaking through the DB schema.
pub const INJECTED_SOURCE_MEMORY_NOTES: &str = "memory_notes";
pub const INJECTED_SOURCE_DUE_NUDGES: &str = "due_nudges";
pub const INJECTED_SOURCE_PROMPT_EXTRA: &str = "prompt_extra";

const SYSTEM_SCAFFOLD: &str = "You are Claw, the system-level agent distributed by the Claw OS project. You may run either inside a full ClawOS installation or as the `claw-os-agent` package installed on another Linux distribution such as Ubuntu. You are not an ordinary app; you operate through native `cos` system primitives.

Host identity:
- Your identity as Claw does not imply that the host operating system is ClawOS.
- When asked which OS or Linux distribution is running, inspect `cos_sysinfo` with `command=info`.
- Call the host ClawOS only when `cos_sysinfo` reports `claw_os: true`. Otherwise name the actual `distribution.pretty_name` and describe Claw as an installed system-agent layer separately.

You operate at two levels:
- System level: processes, memory, disk, network, services, cron, sandboxes, credentials, checkpoints, the policy engine, and the local model runtime — all reachable through `cos_*` tools that mirror the cos CLI exactly.
- Application level: you can also help the user use the apps that run on top of cos.

Tool conventions:
- Each `cos_*` tool takes `{ \"command\": \"<subcommand>\", \"args\": [\"<positional or flag>\", ...] }`. The `command` value is one of the enum entries listed in the tool's input_schema. The `args` array is exactly what the user would type after `cos <primitive> <command>` on the CLI.
- To open a graphical application (Files, Editor, Browser, Terminal, Settings, …) use `cos_app_launcher` — call `find` to resolve a user-spoken name to a freedesktop AppID, then `open` to launch. Never spawn GUI binaries through `cos_app_exec`: the launcher path is gated by the `desktop.launch` capability, honours the user's installed `.desktop` entries (including locale and visibility rules), and detaches the window from the agent's session.
- Destructive operations are gated by the cos `policy` engine. If a primitive returns a policy denial, surface it to the user — do not try to bypass it.
- If a tool errors, read the message carefully, decide whether to retry, change approach, or report back. Never silently re-run a failed destructive command.
- If a tool or App returns `auth_required: true` or `retryable: false`, stop retrying credential/catalog/filesystem tools and explain the single supported next step. Never ask the user to paste a password, access token, or refresh token into chat.

When you respond:
- Be concise. Match the user's language.
- Prefer one decisive answer over hedged options.
- For multi-step jobs, plan briefly, then act. State what you're about to do before issuing destructive tool calls.
- For every claim about observed current system or application state, cite the exact supporting tool call on the same line as `[evidence:<tool_call_id> confidence=<0.00-1.00>]`. Use only tool call IDs from this trajectory. If no runtime evidence supports a system-state claim, state that it is uncertain and identify the missing probe.";

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
    build_system_prompt_for(extra_path, None)
}

/// Like [`build_system_prompt_for`] but also returns the list of
/// auto-injected segments (memory notes, due nudges, extra-file
/// content) so the caller can log them as `injected` rows in the
/// session memory DB. Enforces the "model-visible means logged"
/// invariant: every returned segment appears verbatim inside the
/// returned prompt string.
///
/// The scaffold itself is *not* returned as a segment — it is code,
/// changes only with a release, and would otherwise flood the log
/// on every turn. Only per-turn variable content is captured.
pub fn build_system_prompt_traced(
    extra_path: Option<&Path>,
    query: Option<&str>,
) -> (String, Vec<InjectedSegment>) {
    let mut segments: Vec<InjectedSegment> = Vec::new();
    let mut out = String::from(SYSTEM_SCAFFOLD);

    if let Some(notes) = NotesStore::system_default().assemble_for_prompt_relevant(
        query,
        crate::agent::memory::notes::MAX_NOTE_CHARS_FOR_PROMPT,
    ) {
        let wrapped = crate::agent::safety::untrusted::wrap_untrusted(
            crate::agent::safety::untrusted::MEMORY_TAG,
            &notes,
        );
        out.push_str("\n\n---\n\n");
        out.push_str(&wrapped);
        segments.push(InjectedSegment {
            source: INJECTED_SOURCE_MEMORY_NOTES,
            content: wrapped,
        });
    }

    let store = NudgeStore::new(crate::paths::agent_nudges_path());
    let due = store.due(now_epoch_s());
    if !due.is_empty() {
        let mut block = String::from("<DUE_NUDGES>\n");
        for n in &due {
            block.push_str(&format!("- [{}] {}\n", n.id, n.message));
        }
        block.push_str("</DUE_NUDGES>");
        out.push_str("\n\n---\n\n");
        out.push_str(&block);
        segments.push(InjectedSegment {
            source: INJECTED_SOURCE_DUE_NUDGES,
            content: block,
        });
    }

    if let Some(p) = extra_path {
        const MAX_PROMPT_EXTRA_BYTES: u64 = 256 * 1024;
        let meta = fs::metadata(p).ok();
        let len_ok = meta.as_ref().map(|m| m.len() <= MAX_PROMPT_EXTRA_BYTES).unwrap_or(false);
        if len_ok {
            if let Ok(extra) = fs::read_to_string(p) {
                let trimmed = extra.trim_end();
                if !trimmed.is_empty() {
                    out.push_str("\n\n---\n\n");
                    out.push_str(trimmed);
                    segments.push(InjectedSegment {
                        source: INJECTED_SOURCE_PROMPT_EXTRA,
                        content: trimmed.to_string(),
                    });
                }
            }
        }
    }

    (out, segments)
}

/// Assert (in debug builds) that every recorded injected segment
/// appears verbatim in the assembled prompt. Enforces the
/// "model-visible means logged" invariant at the build seam: if a
/// future change adds a new injection path but forgets to record it,
/// this fires in tests before it reaches production.
#[cfg(debug_assertions)]
fn assert_segments_visible(prompt: &str, segments: &[InjectedSegment]) {
    for seg in segments {
        debug_assert!(
            prompt.contains(&seg.content),
            "injected segment {:?} not present in assembled prompt",
            seg.source,
        );
    }
}

/// Like [`build_system_prompt`] but selects relevance-ranked memory for
/// the current turn. `query` is the user's message; when memory exceeds
/// the prompt budget, only the entries most relevant to it are injected
/// (always-on entries and USER.md are kept regardless). Pass `None` for
/// turn-agnostic assembly (e.g. diagnostics).
pub fn build_system_prompt_for(extra_path: Option<&Path>, query: Option<&str>) -> String {
    // Single source of truth: assemble via the traced variant and
    // drop the segment list. Callers that need to log injections
    // should call `build_system_prompt_traced` directly.
    let (prompt, segments) = build_system_prompt_traced(extra_path, query);
    #[cfg(debug_assertions)]
    assert_segments_visible(&prompt, &segments);
    #[cfg(not(debug_assertions))]
    let _ = segments;
    prompt
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/prompt.rs"
    ));
}

//! AI subsystem configuration (serializable, part of cosmic-term config).

use serde::{Deserialize, Serialize};

/// AI configuration nested under cosmic-term's main config.
///
/// Ported from `aterm-ai/src/config.ts` + `aiSettingsTab.component.ts` — fields
/// the user can tweak via Settings → AI.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiConfig {
    /// Master toggle. When false, `@` is treated as a regular character.
    pub enabled: bool,

    /// Maximum number of recent terminal lines to include as context.
    /// Aterm default: 100 (MAX_CONTEXT_LINES in aiMiddleware.ts).
    pub max_context_lines: usize,

    /// Multi-line paste collapse threshold (lines).
    /// Pastes with >= this many newlines are folded into a placeholder.
    /// Aterm default: 1 (LARGE_PASTE_LINE_THRESHOLD).
    pub paste_line_threshold: usize,

    /// Multi-line paste collapse threshold (characters).
    /// Pastes with >= this many chars are folded into a placeholder.
    /// Aterm default: 300 (LARGE_PASTE_CHAR_THRESHOLD).
    pub paste_char_threshold: usize,

    /// Optional model override. When empty, copilot CLI uses its default.
    pub model: String,

    /// Pass `--allow-all-tools` to copilot. When false, copilot will prompt
    /// the user inline before each destructive tool call.
    pub allow_all_tools: bool,

    /// Optional extra arguments appended to the copilot command, space-split
    /// at shell expansion time. Useful for `--effort xhigh`, MCP configs, etc.
    pub extra_args: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_context_lines: 100,
            paste_line_threshold: 1,
            paste_char_threshold: 300,
            model: String::new(),
            allow_all_tools: true,
            extra_args: String::new(),
        }
    }
}

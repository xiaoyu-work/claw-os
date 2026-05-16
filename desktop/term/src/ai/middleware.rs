//! AI input state machine — port of `aterm-ai/src/aiMiddleware.ts`.
//!
//! ## State machine
//!
//! - [`State::Normal`]: keystrokes pass through to the PTY untouched.
//!   The middleware tracks `input_length` (chars typed since the last newline)
//!   so we can detect "line start" (input_length == 0).
//! - [`State::Pending`]: user typed `@` at line start. We've drawn the colored
//!   `@` to the grid (purely cosmetic — not sent to PTY). The next byte decides:
//!   space → enter capturing mode; anything else → cancel and forward both.
//! - [`State::Capturing`]: we own the input line. Each keystroke either
//!   accumulates in the prompt buffer, performs editing (backspace), or
//!   submits (Enter) / cancels (Ctrl+C / Esc). Bracketed paste blocks are
//!   absorbed atomically.
//!
//! ## Visual rendering
//!
//! Unlike aterm (which writes directly to xterm.js), we own the alacritty
//! `Term`. The middleware keeps its own `vte::ansi::Processor` so it can advance
//! the Term with display bytes (`\r\x1b[2K\x1b[36m@ \x1b[39m<text>`) without
//! touching the PTY. This is safe because the EventLoop's parser is independent
//! of ours — both parsers can mutate Term grid state in interleaved chunks.
//!
//! ## Alt-screen pass-through
//!
//! When a TUI (vim, htop, claude-code) switches to the alt screen
//! (`\x1b[?1049h`), the middleware becomes transparent until the app exits
//! (`\x1b[?1049l`). The EventLoop already tracks this via `TermMode::ALT_SCREEN`
//! — we read it directly from the locked Term.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;
use std::collections::HashMap;

use super::config::AiConfig;

const PROMPT_PREFIX: &str = "\r\x1b[2K\x1b[36m@ \x1b[39m";

/// Single character used to mark a placeholder for a folded paste.
fn paste_placeholder(id: usize, lines: usize, chars: usize) -> String {
    if lines > 1 {
        format!("[Pasted Text: {lines} lines #{id}]")
    } else {
        format!("[Pasted Text: {chars} chars #{id}]")
    }
}

/// Outcome of feeding bytes into the middleware. The caller (Terminal) decides
/// how to act: forward to PTY, drop, or perform submission.
pub enum AiAction {
    /// Bytes were consumed entirely by the middleware. Do nothing.
    Absorb,
    /// Forward `bytes` to the PTY as-is. (`Normal` state passthrough, or
    /// `Pending`→`Normal` cancellation that flushes the buffered `@`.)
    Forward(Vec<u8>),
    /// User pressed Enter on a non-empty AI prompt. Caller must:
    /// 1. Write `prompt` to `$COS_AI_TMP/aq-<id>.txt`.
    /// 2. Snapshot context and write to `$COS_AI_TMP/ac-<id>.json`.
    /// 3. Inject ` __cos_ai <id>\r` to the PTY (leading space hides from history).
    Submit { id: String, prompt: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Normal,
    Pending,
    Capturing,
}

pub struct AiMiddleware {
    state: State,
    /// Buffer of characters captured after `@ `. Includes paste placeholders.
    prompt: String,
    /// Number of chars typed since the last newline (for line-start detection).
    /// Tracks the *shell's* notion of cursor column (best-effort).
    input_length: usize,
    /// Display-text → original-content map for folded pastes.
    pasted_content: HashMap<String, String>,
    next_paste_id: usize,
    /// Counter for generating short query IDs.
    next_query_id: usize,
    /// Our own ANSI processor for injecting display bytes into Term.
    display_parser: Processor,
    /// User config (paste thresholds, enabled, etc.).
    config: AiConfig,
}

impl AiMiddleware {
    pub fn new(config: AiConfig) -> Self {
        Self {
            state: State::Normal,
            prompt: String::new(),
            input_length: 0,
            pasted_content: HashMap::new(),
            next_paste_id: 1,
            next_query_id: 1,
            display_parser: Processor::new(),
            config,
        }
    }

    pub fn config(&self) -> &AiConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: AiConfig) {
        self.config = config;
    }

    /// True if the middleware currently owns the input line.
    pub fn is_capturing(&self) -> bool {
        self.state == State::Capturing
    }

    /// Reset to Normal state. Call this when the terminal session resets,
    /// the user clears the screen, or after submission completes.
    pub fn reset(&mut self) {
        self.state = State::Normal;
        self.prompt.clear();
        self.input_length = 0;
        self.pasted_content.clear();
    }

    /// Called when PTY output reaches the terminal — used to keep
    /// `input_length` in sync. A newline / CR means the cursor moved to a new
    /// line so we're at line start again.
    ///
    /// This is intentionally simple — aterm uses the same heuristic
    /// (`feedFromSession` line 380-382).
    pub fn observe_output(&mut self, data: &[u8]) {
        for &b in data {
            if b == b'\n' || b == b'\r' {
                self.input_length = 0;
            }
        }
    }

    /// Feed user input bytes through the state machine.
    ///
    /// `term` is the locked alacritty Term used for cosmetic redraw of the
    /// capturing prompt. `clipboard` is the current clipboard text (used for
    /// Ctrl+V handling).
    ///
    /// Aterm reference: `feedFromTerminal` (aiMiddleware.ts:407).
    pub fn feed_input<T: EventListener>(
        &mut self,
        data: &[u8],
        term: &mut Term<T>,
        clipboard: Option<&str>,
    ) -> AiAction {
        if !self.config.enabled {
            return AiAction::Forward(data.to_vec());
        }

        // Alt-screen pass-through. TUI apps (vim/claude) handle their own input.
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return AiAction::Forward(data.to_vec());
        }

        // Multi-byte input (paste, escape sequence, IME) — handle as one chunk.
        if data.len() != 1 {
            return self.feed_multi(data, term, clipboard);
        }

        let byte = data[0];
        match self.state {
            State::Normal => self.feed_normal_single(byte, term),
            State::Pending => self.feed_pending_single(byte, term, clipboard),
            State::Capturing => self.feed_capturing(data, term, clipboard),
        }
    }

    // ────────────────────────── Normal state ──────────────────────────

    fn feed_normal_single<T: EventListener>(
        &mut self,
        byte: u8,
        term: &mut Term<T>,
    ) -> AiAction {
        // `@` at line start → enter Pending, draw colored `@`, don't forward yet.
        if byte == b'@' && self.input_length == 0 {
            self.state = State::Pending;
            self.write_display(term, b"\x1b[36m@\x1b[39m");
            return AiAction::Absorb;
        }

        self.update_input_length(byte);
        AiAction::Forward(vec![byte])
    }

    fn update_input_length(&mut self, byte: u8) {
        match byte {
            b'\r' | b'\n' => self.input_length = 0,
            // Backspace / DEL
            0x08 | 0x7F => self.input_length = self.input_length.saturating_sub(1),
            // Ctrl+C / Ctrl+U → assume shell clears the line.
            0x03 | 0x15 => self.input_length = 0,
            b if b >= 0x20 => self.input_length += 1,
            _ => {}
        }
    }

    // ────────────────────────── Pending state ──────────────────────────

    fn feed_pending_single<T: EventListener>(
        &mut self,
        byte: u8,
        term: &mut Term<T>,
        clipboard: Option<&str>,
    ) -> AiAction {
        match byte {
            // Space after `@` → enter capturing mode. Don't forward.
            b' ' => {
                self.state = State::Capturing;
                self.prompt.clear();
                self.render_capturing(term);
                AiAction::Absorb
            }
            // Backspace → cancel, redraw clean prompt, no PTY send.
            0x7F | 0x08 => {
                self.state = State::Normal;
                self.write_display(term, b"\r\x1b[2K");
                self.input_length = 0;
                // Re-send the prompt redraw by signalling \r to shell.
                AiAction::Forward(vec![b'\r'])
            }
            // Ctrl+V → if clipboard has text, jump straight to capturing with that
            // text. Mirrors aterm aiMiddleware.ts:483-491.
            0x16 => {
                if let Some(pasted) = clipboard.filter(|s| !s.is_empty()) {
                    self.state = State::Capturing;
                    self.prompt.clear();
                    let display = self.maybe_collapse_paste(pasted);
                    self.prompt.push_str(&display);
                    self.render_capturing(term);
                    AiAction::Absorb
                } else {
                    AiAction::Absorb
                }
            }
            // Anything else → cancel @ mode, forward `@` + this byte as if user
            // had typed them normally. (e.g., `@gmail.com` should still work.)
            _ => {
                self.state = State::Normal;
                // Erase our cosmetic `@` from the grid by redrawing clean.
                self.write_display(term, b"\r\x1b[2K");
                self.input_length = 1; // the literal '@' the shell will echo
                AiAction::Forward(vec![b'@', byte])
            }
        }
    }

    // ────────────────────────── Capturing state ──────────────────────────

    /// Aterm's `applyCapturingText` (aiMiddleware.ts:100-175) — handles a chunk
    /// of bytes while in capturing mode. Supports embedded bracketed paste,
    /// backspace, Enter, Ctrl+C/Esc, Ctrl+V.
    fn feed_capturing<T: EventListener>(
        &mut self,
        data: &[u8],
        term: &mut Term<T>,
        clipboard: Option<&str>,
    ) -> AiAction {
        let raw = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return AiAction::Absorb,
        };

        // Bracketed paste block: \x1b[200~...\x1b[201~. Aterm splits on the marker
        // and folds large pastes into placeholders.
        if let Some(rest) = raw.strip_prefix("\x1b[200~") {
            if let Some(end_idx) = rest.find("\x1b[201~") {
                let pasted = &rest[..end_idx];
                let after = &rest[end_idx + "\x1b[201~".len()..];
                let cleaned = pasted.replace("\r\n", "\n").replace('\r', "\n");
                if !cleaned.is_empty() {
                    let display = self.maybe_collapse_paste(&cleaned);
                    self.prompt.push_str(&display);
                    self.render_capturing(term);
                }
                if !after.is_empty() {
                    // Recurse on the tail (likely an Enter key from paste-on-submit).
                    return self.feed_capturing(after.as_bytes(), term, clipboard);
                }
                return AiAction::Absorb;
            }
            // Incomplete paste — wait for more (drop for now, mirrors aterm).
            return AiAction::Absorb;
        }

        // Strip CSI / SS3 escape sequences (arrow keys, etc.) — aterm line 121-122.
        let cleaned = strip_csi_ss3(raw);
        if cleaned.is_empty() {
            return AiAction::Absorb;
        }

        let mut changed = false;
        for ch in cleaned.chars() {
            match ch {
                // Ctrl+V → clipboard paste
                '\u{16}' => {
                    if let Some(p) = clipboard.filter(|s| !s.is_empty()) {
                        let display = self.maybe_collapse_paste(p);
                        self.prompt.push_str(&display);
                        changed = true;
                    }
                }
                // Enter → submit
                '\r' => {
                    self.write_display(term, b"\r\n");
                    return self.submit();
                }
                '\n' => {
                    // Bare \n inside a single keystroke shouldn't happen; ignore.
                }
                // Backspace / DEL
                '\u{7F}' | '\u{08}' => {
                    if self.prompt.is_empty() {
                        // Exit capture entirely if user backspaces through empty buffer.
                        self.state = State::Normal;
                        self.write_display(term, b"\r\x1b[2K");
                        self.input_length = 0;
                        return AiAction::Forward(vec![b'\r']);
                    }
                    pop_char(&mut self.prompt);
                    changed = true;
                }
                // Ctrl+C / Esc → abort
                '\u{03}' | '\u{1B}' => {
                    self.state = State::Normal;
                    self.prompt.clear();
                    self.pasted_content.clear();
                    self.write_display(term, b"\r\n");
                    self.input_length = 0;
                    return AiAction::Forward(vec![b'\r']);
                }
                _ => {
                    self.prompt.push(ch);
                    changed = true;
                }
            }
        }

        if changed {
            self.render_capturing(term);
        }
        AiAction::Absorb
    }

    // ────────────────────────── Submission ──────────────────────────

    fn submit(&mut self) -> AiAction {
        let raw_prompt = self.prompt.trim().to_string();
        self.prompt.clear();
        self.state = State::Normal;
        self.input_length = 0;

        if raw_prompt.is_empty() {
            self.pasted_content.clear();
            return AiAction::Forward(vec![b'\r']);
        }

        // Expand paste placeholders back to full content.
        let expanded = if self.pasted_content.is_empty() {
            raw_prompt
        } else {
            let mut out = raw_prompt;
            for (placeholder, content) in self.pasted_content.drain() {
                out = out.replace(&placeholder, &content);
            }
            out
        };

        // Short random-ish ID (matches aterm's 6-char base36 style).
        let id = format!("{:x}", self.next_query_id.wrapping_mul(2654435761));
        self.next_query_id = self.next_query_id.wrapping_add(1);

        AiAction::Submit { id, prompt: expanded }
    }

    // ────────────────────────── Multi-byte chunks ──────────────────────────

    fn feed_multi<T: EventListener>(
        &mut self,
        data: &[u8],
        term: &mut Term<T>,
        clipboard: Option<&str>,
    ) -> AiAction {
        // Bracketed-paste markers may straddle states. Aterm handles a chunk
        // starting in Pending or Normal by stripping markers and treating
        // payload as text. We do the same.
        let raw = match std::str::from_utf8(data) {
            Ok(s) => s,
            Err(_) => {
                // Invalid UTF-8 — forward unchanged (input_length update done crudely).
                for &b in data {
                    self.update_input_length(b);
                }
                return AiAction::Forward(data.to_vec());
            }
        };

        if self.state == State::Capturing {
            return self.feed_capturing(data, term, clipboard);
        }

        // Strip bracketed-paste markers and detect content.
        let stripped: String = raw.replace("\x1b[200~", "").replace("\x1b[201~", "");
        let cleaned = strip_csi_ss3(&stripped);

        if self.state == State::Pending {
            if cleaned.is_empty() {
                return AiAction::Absorb;
            }
            // Switch to Capturing using the pasted text as the initial prompt
            // (mirrors aterm:432-449).
            self.state = State::Capturing;
            self.prompt.clear();
            let pasted = cleaned
                .strip_prefix(' ')
                .unwrap_or(&cleaned)
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            if !pasted.is_empty() {
                let display = self.maybe_collapse_paste(&pasted);
                self.prompt.push_str(&display);
            }
            self.render_capturing(term);
            return AiAction::Absorb;
        }

        // Normal state: forward but keep input_length in sync.
        if !cleaned.is_empty() {
            // ESC sequences shouldn't bump input_length; we've already stripped them.
            for ch in cleaned.chars() {
                if ch as u32 >= 0x20 {
                    self.input_length += 1;
                } else if ch == '\r' || ch == '\n' {
                    self.input_length = 0;
                }
            }
        }
        AiAction::Forward(data.to_vec())
    }

    // ────────────────────────── Helpers ──────────────────────────

    fn maybe_collapse_paste(&mut self, text: &str) -> String {
        let line_count = text.matches('\n').count() + 1;
        let char_count = text.chars().count();
        let big = line_count > self.config.paste_line_threshold
            || char_count >= self.config.paste_char_threshold;

        if !big {
            return text.replace('\n', " ");
        }

        let id = self.next_paste_id;
        self.next_paste_id += 1;
        let placeholder = paste_placeholder(id, line_count, char_count);
        self.pasted_content
            .insert(placeholder.clone(), text.to_string());
        placeholder
    }

    fn render_capturing<T: EventListener>(&mut self, term: &mut Term<T>) {
        // Highlight paste placeholders in yellow (aterm renderCapturingPrompt).
        let mut out = String::from(PROMPT_PREFIX);
        let prompt = self.prompt.clone();
        for (idx, segment) in split_with_placeholders(&prompt).into_iter().enumerate() {
            if idx % 2 == 1 {
                // Placeholder segment → yellow.
                out.push_str("\x1b[33m");
                out.push_str(&segment);
                out.push_str("\x1b[39m");
            } else {
                out.push_str(&segment);
            }
        }
        self.write_display(term, out.as_bytes());
    }

    /// Push bytes through our private parser into the alacritty Term grid
    /// without sending them to the PTY. Safe to interleave with the
    /// EventLoop's parser writes — they share the Term but not parser state.
    fn write_display<T: EventListener>(&mut self, term: &mut Term<T>, bytes: &[u8]) {
        self.display_parser.advance(term, bytes);
    }
}

/// Remove only single-char ANSI sequences common in keyboard input:
/// CSI (`\x1b[...letter`) and SS3 (`\x1bO.`). Used to drop arrow keys and
/// function keys while keeping printable text.
fn strip_csi_ss3(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    // CSI: \x1b [ params... final
                    let mut j = i + 2;
                    while j < bytes.len() && !(0x40..=0x7E).contains(&bytes[j]) {
                        j += 1;
                    }
                    i = j.saturating_add(1).min(bytes.len());
                }
                b'O' => {
                    // SS3: \x1b O <one byte>
                    i = (i + 3).min(bytes.len());
                }
                _ => {
                    // Lone ESC — preserve as-is (will be caught upstream as cancel).
                    out.push(0x1B as char);
                    i += 1;
                }
            }
        } else {
            // Find the next escape and bulk-copy.
            let next = bytes[i..].iter().position(|&b| b == 0x1B).map(|p| i + p);
            let end = next.unwrap_or(bytes.len());
            // SAFETY: input was UTF-8; we copy a prefix that ends at a byte
            // boundary because we only break on ESC (a single ASCII byte).
            out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or(""));
            i = end;
        }
    }
    out
}

/// Pop the last `char` from a String (UTF-8 aware).
fn pop_char(s: &mut String) {
    if let Some(ch) = s.chars().last() {
        let new_len = s.len() - ch.len_utf8();
        s.truncate(new_len);
    }
}

/// Split a string on paste placeholders (`[Pasted Text: N (lines|chars) #N]`).
/// Returns alternating segments: text, placeholder, text, placeholder, …
/// The caller can colorize odd-indexed segments.
fn split_with_placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let bytes = s.as_bytes();
    while cursor < bytes.len() {
        if let Some(start) = s[cursor..].find("[Pasted Text:") {
            let abs_start = cursor + start;
            if let Some(end_rel) = s[abs_start..].find(']') {
                let abs_end = abs_start + end_rel + 1;
                out.push(s[cursor..abs_start].to_string());
                out.push(s[abs_start..abs_end].to_string());
                cursor = abs_end;
                continue;
            }
        }
        out.push(s[cursor..].to_string());
        break;
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_csi_removes_arrow_keys() {
        assert_eq!(strip_csi_ss3("\x1b[A"), "");
        assert_eq!(strip_csi_ss3("\x1b[Bhello"), "hello");
        assert_eq!(strip_csi_ss3("hi\x1b[Cworld"), "hiworld");
        assert_eq!(strip_csi_ss3("\x1bOP"), "");
        assert_eq!(strip_csi_ss3("plain"), "plain");
    }

    #[test]
    fn pop_char_handles_unicode() {
        let mut s = String::from("héllo");
        pop_char(&mut s);
        assert_eq!(s, "héll");
        let mut s = String::from("a你");
        pop_char(&mut s);
        assert_eq!(s, "a");
    }

    #[test]
    fn split_placeholders_basic() {
        let out = split_with_placeholders("hello [Pasted Text: 3 lines #1] world");
        assert_eq!(out, vec!["hello ", "[Pasted Text: 3 lines #1]", " world"]);
    }

    #[test]
    fn split_placeholders_no_match() {
        let out = split_with_placeholders("just text");
        assert_eq!(out, vec!["just text"]);
    }
}

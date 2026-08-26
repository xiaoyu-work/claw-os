//! Parse `@`-prefixed file/URL references out of a user message.
//!
//! Convention (matches the user-facing CLI/TUI):
//!
//!   * `@path/to/file.txt`        — local path (no spaces)
//!   * `@"path with spaces.txt"`  — local path with spaces, quoted
//!   * `@https://example.com/x`   — absolute URL (http or https)
//!   * `@./relative/path`         — explicitly relative path
//!
//! The parser is a *pure-functional* token extractor. It does no
//! IO, does no scope/policy validation, and does not deduplicate
//! within the input — callers are expected to do those steps with
//! the [`Reference`] list.
//!
//! Edge cases the parser intentionally handles:
//!
//!   * `email@example.com` — the `@` is preceded by a non-whitespace
//!     character, so it is **not** a reference. (Same rule as
//!     `@user` in chat: must be at start of input or preceded by
//!     whitespace / a punctuation boundary.)
//!   * Markdown links `[text](path)` — `@` inside link targets is
//!     extracted normally; we don't do markdown-aware parsing.
//!   * Backtick spans — refs inside backticks are also extracted.
//!     The CLI/TUI layer can decide to honour or ignore them.
//!   * `@@foo` (double `@`) — first `@` is a literal escape; we
//!     skip the doubled prefix and emit `@foo` as a literal,
//!     producing **no** reference.
//!   * Trailing punctuation (`,.;:!?)`]`) is stripped from the
//!     reference body so `Read @notes.md.` works.
//!
//! Output preserves source order; duplicate refs are kept (caller
//! decides whether to dedupe).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The raw text after the `@`, with quotes stripped and trailing
    /// punctuation removed.
    pub raw: String,
    pub kind: ReferenceKind,
    /// Byte offset of the leading `@` in the original input.
    pub start: usize,
    /// Byte offset just past the end of the matched span (after
    /// closing quote, if any) in the original input.
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReferenceKind {
    /// `@http://...` or `@https://...`
    Url,
    /// `@./...` or `@../...` — explicitly relative.
    RelativePath,
    /// `@/...` — absolute path (Unix-style).
    AbsolutePath,
    /// Anything else: bare path-like token.
    Path,
}

/// Classify a reference body into a [`ReferenceKind`].
fn classify(body: &str) -> ReferenceKind {
    let lower = body.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        ReferenceKind::Url
    } else if body.starts_with("./") || body.starts_with("../") {
        ReferenceKind::RelativePath
    } else if body.starts_with('/') || (body.len() >= 3 && is_drive_prefix(body)) {
        ReferenceKind::AbsolutePath
    } else {
        ReferenceKind::Path
    }
}

/// Detects Windows drive prefixes like `C:\` or `c:/`.
fn is_drive_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Trailing punctuation chars stripped from non-quoted refs so
/// `Read @notes.md.` doesn't end up referencing `notes.md.` (which
/// is rarely intended).
const TRIM_TAIL: &[char] = &[
    ',', '.', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '`',
];

/// Extract every `@`-reference from `text`, in source order.
pub fn extract(text: &str) -> Vec<Reference> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }

        // Boundary check: `@` must be at start-of-input or follow
        // whitespace / punctuation, NOT a word character. This is
        // what makes `email@example.com` not match.
        if i > 0 {
            let prev = bytes[i - 1];
            let is_word_prev = prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.';
            if is_word_prev {
                i += 1;
                continue;
            }
        }

        // Escape: `@@` -> literal @, no reference emitted.
        if i + 1 < bytes.len() && bytes[i + 1] == b'@' {
            i += 2;
            continue;
        }

        let start = i;
        // Quoted form: @"..."
        if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
            // Find the closing quote.
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b'"' {
                j += 1;
            }
            if j < bytes.len() {
                // Emit the body (between the quotes).
                let body = &text[i + 2..j];
                if !body.is_empty() {
                    let body_owned = body.to_string();
                    let kind = classify(&body_owned);
                    out.push(Reference {
                        raw: body_owned,
                        kind,
                        start,
                        end: j + 1, // include closing quote
                    });
                }
                i = j + 1;
                continue;
            } else {
                // Unterminated quote — bail out, advance one byte.
                i += 1;
                continue;
            }
        }

        // Bare form: @<token>
        let mut j = i + 1;
        while j < bytes.len() {
            let c = bytes[j];
            if c.is_ascii_whitespace() {
                break;
            }
            j += 1;
        }
        let mut body = &text[i + 1..j];
        // Strip trailing punctuation (URL tail, sentence end, etc.)
        // but keep going until we either consume all trim chars or
        // run into something else.
        let mut end = j;
        while !body.is_empty() {
            // Avoid stripping the final char of an empty path like `@.`
            if let Some(last) = body.chars().last() {
                if TRIM_TAIL.contains(&last) {
                    let new_end = end - last.len_utf8();
                    body = &body[..body.len() - last.len_utf8()];
                    end = new_end;
                    continue;
                }
            }
            break;
        }
        if !body.is_empty() {
            let body_owned = body.to_string();
            let kind = classify(&body_owned);
            out.push(Reference {
                raw: body_owned,
                kind,
                start,
                end,
            });
        }
        i = j;
    }
    out
}

/// Same as [`extract`] but de-duplicates (`raw` + `kind`) while
/// preserving the order of first appearance.
pub fn extract_unique(text: &str) -> Vec<Reference> {
    let all = extract(text);
    let mut seen: std::collections::BTreeSet<(String, ReferenceKind)> =
        std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(all.len());
    for r in all {
        let key = (r.raw.clone(), r.kind);
        if seen.insert(key) {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/context/references.rs"
    ));
}

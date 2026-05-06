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
const TRIM_TAIL: &[char] = &[',', '.', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '`'];

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
    use super::*;

    #[test]
    fn extracts_simple_path_reference() {
        let refs = extract("please read @notes.md");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "notes.md");
        assert_eq!(refs[0].kind, ReferenceKind::Path);
    }

    #[test]
    fn classifies_url_reference() {
        let refs = extract("see @https://example.com/page");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "https://example.com/page");
        assert_eq!(refs[0].kind, ReferenceKind::Url);
    }

    #[test]
    fn classifies_relative_and_absolute_paths() {
        let refs = extract("a @./local b @/etc/hosts c @../up");
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].kind, ReferenceKind::RelativePath);
        assert_eq!(refs[1].kind, ReferenceKind::AbsolutePath);
        assert_eq!(refs[2].kind, ReferenceKind::RelativePath);
    }

    #[test]
    fn classifies_windows_drive_paths() {
        let refs = extract("look at @C:\\Users\\me\\file.txt");
        assert_eq!(refs.len(), 1);
        // Windows path: trailing chars are not in TRIM_TAIL until '.txt' so
        // the 't' isn't stripped. End-trim only fires on punctuation.
        assert_eq!(refs[0].raw, "C:\\Users\\me\\file.txt");
        assert_eq!(refs[0].kind, ReferenceKind::AbsolutePath);
    }

    #[test]
    fn skips_email_addresses() {
        let refs = extract("ping me at user@example.com please");
        assert!(refs.is_empty(), "got: {refs:?}");
    }

    #[test]
    fn skips_doubled_at_escape() {
        let refs = extract("write @@foo as a literal");
        assert!(refs.is_empty(), "got: {refs:?}");
    }

    #[test]
    fn quoted_path_keeps_spaces() {
        let refs = extract("look at @\"my notes.md\" please");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "my notes.md");
        assert_eq!(refs[0].kind, ReferenceKind::Path);
    }

    #[test]
    fn quoted_unterminated_emits_nothing() {
        let refs = extract("look at @\"notes.md without close quote");
        assert!(refs.is_empty(), "got: {refs:?}");
    }

    #[test]
    fn empty_quoted_emits_nothing() {
        let refs = extract("@\"\" ignored");
        assert!(refs.is_empty(), "got: {refs:?}");
    }

    #[test]
    fn trims_trailing_sentence_punctuation() {
        let refs = extract("Read @notes.md, then @summary.txt.");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].raw, "notes.md");
        assert_eq!(refs[1].raw, "summary.txt");
    }

    #[test]
    fn trims_trailing_brackets_and_parens() {
        let refs = extract("(see @doc.md) and [also @other.md]");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].raw, "doc.md");
        assert_eq!(refs[1].raw, "other.md");
    }

    #[test]
    fn keeps_url_query_and_fragment() {
        let refs = extract("@https://example.com/x?a=1&b=2#frag");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "https://example.com/x?a=1&b=2#frag");
    }

    #[test]
    fn at_start_of_input_works() {
        let refs = extract("@README.md");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "README.md");
    }

    #[test]
    fn span_offsets_match_original_text() {
        let s = "x @abc";
        let refs = extract(s);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].start, 2);
        assert_eq!(refs[0].end, 6);
        assert_eq!(&s[refs[0].start..refs[0].end], "@abc");
    }

    #[test]
    fn quoted_span_includes_closing_quote() {
        let s = "@\"a b\"";
        let refs = extract(s);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].start, 0);
        assert_eq!(refs[0].end, s.len());
        assert_eq!(refs[0].raw, "a b");
    }

    #[test]
    fn multiple_references_preserve_order_and_offsets() {
        let s = "@one and @two and @three";
        let refs = extract(s);
        let raws: Vec<&str> = refs.iter().map(|r| r.raw.as_str()).collect();
        assert_eq!(raws, vec!["one", "two", "three"]);
        assert!(refs[0].start < refs[1].start);
        assert!(refs[1].start < refs[2].start);
    }

    #[test]
    fn extract_unique_dedupes_preserving_first_position() {
        let s = "@a @b @a @b @c";
        let refs = extract_unique(s);
        let raws: Vec<&str> = refs.iter().map(|r| r.raw.as_str()).collect();
        assert_eq!(raws, vec!["a", "b", "c"]);
    }

    #[test]
    fn extract_unique_treats_different_kinds_as_distinct() {
        let s = "@./README.md and @README.md";
        let refs = extract_unique(s);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].kind, ReferenceKind::RelativePath);
        assert_eq!(refs[1].kind, ReferenceKind::Path);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(extract("").is_empty());
        assert!(extract_unique("").is_empty());
    }

    #[test]
    fn no_references_returns_empty() {
        assert!(extract("just plain text, no refs").is_empty());
    }

    #[test]
    fn lone_at_with_no_body_emits_nothing() {
        let refs = extract("hello @ world");
        assert!(refs.is_empty(), "got: {refs:?}");
    }

    #[test]
    fn url_classification_is_case_insensitive() {
        let refs = extract("@HTTPS://EXAMPLE.COM/x");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, ReferenceKind::Url);
    }

    #[test]
    fn ref_inside_backticks_still_extracted() {
        let refs = extract("see `@notes.md` for details");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].raw, "notes.md");
    }

    #[test]
    fn boundary_after_period_in_word_does_not_trigger_ref() {
        // Regression guard: 'foo.com@bar' should not split into a ref
        // because the '@' is preceded by a word/period character.
        let refs = extract("foo.com@bar");
        assert!(refs.is_empty(), "got: {refs:?}");
    }
}

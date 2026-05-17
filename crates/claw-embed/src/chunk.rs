//! Sliding-window text chunker.
//!
//! Splits a document's plain text into overlapping windows roughly
//! `chunk_chars` long with `overlap_chars` of overlap between
//! adjacent windows. We chunk on Unicode codepoint boundaries rather
//! than raw byte offsets so we don't slice multibyte characters.

use serde::{Deserialize, Serialize};

/// A document chunk waiting to be embedded.
///
/// We chunk before embedding because:
/// 1. Most embedding models have a 512–8K-token context cap.
/// 2. Fine-grained chunks let us return precise snippets in search hits
///    instead of just "this 80-page PDF is relevant".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Absolute path to the source file.
    pub path: String,
    /// 0-based ordinal within the file. Two chunks with the same `(path,
    /// chunk_id)` represent the same logical span across reindex cycles.
    pub chunk_id: u32,
    /// Plain UTF-8 text of this chunk, after extraction.
    pub text: String,
}

/// Produce overlapping chunks for one document.
///
/// Returns an empty Vec for empty or whitespace-only input.
pub fn chunks_for(
    path: &str,
    text: &str,
    chunk_chars: usize,
    overlap_chars: usize,
) -> Vec<Chunk> {
    let chunk_chars = chunk_chars.max(64);
    let overlap_chars = overlap_chars.min(chunk_chars / 2);

    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let step = chunk_chars - overlap_chars;
    let mut start = 0usize;
    let mut id: u32 = 0;
    while start < chars.len() {
        let end = (start + chunk_chars).min(chars.len());
        let slice: String = chars[start..end].iter().collect();
        if !slice.trim().is_empty() {
            out.push(Chunk {
                path: path.to_string(),
                chunk_id: id,
                text: slice,
            });
            id += 1;
        }
        if end == chars.len() {
            break;
        }
        start += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunks_for("/a", "", 100, 10).is_empty());
        assert!(chunks_for("/a", "   \n\t  ", 100, 10).is_empty());
    }

    #[test]
    fn short_input_yields_one_chunk() {
        let cs = chunks_for("/a", "hello world", 100, 10);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].chunk_id, 0);
        assert_eq!(cs[0].text, "hello world");
    }

    #[test]
    fn overlap_is_respected() {
        let text: String = (0..200).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        let cs = chunks_for("/a", &text, 100, 20);
        assert_eq!(cs.len(), 3);
        assert!(cs[0].text.len() == 100);
        let tail0: String = cs[0]
            .text
            .chars()
            .rev()
            .take(20)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let head1: String = cs[1].text.chars().take(20).collect();
        assert_eq!(tail0, head1);
    }

    #[test]
    fn multibyte_chars_are_not_split() {
        let text = "中文测试一二三四五六七八九十".repeat(20);
        let cs = chunks_for("/a", &text, 30, 5);
        for c in &cs {
            assert!(c.text.is_char_boundary(0));
            assert!(c.text.is_char_boundary(c.text.len()));
        }
    }
}

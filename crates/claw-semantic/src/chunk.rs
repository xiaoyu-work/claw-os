//! Sliding-window text chunker.
//!
//! Splits a document's plain text into overlapping windows roughly
//! `chunk_chars` long with `overlap_chars` of overlap between
//! adjacent windows. We chunk on Unicode grapheme boundaries rather
//! than raw byte offsets so we don't slice multibyte characters.

use crate::Chunk;

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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/chunk.rs"
    ));
}

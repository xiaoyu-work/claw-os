//! Text extraction: bytes-on-disk → plain UTF-8 string.
//!
//! Phase 1 ships [`TextExtractor`] which handles .txt / .md / .rst /
//! source-code-ish files (anything we can decode as UTF-8). Binary
//! formats (pdf, docx, …) come in Phase 2 via dedicated crates
//! (`pdf-extract`, `docx-rs`, etc.) — each will implement the
//! [`Extractor`] trait below and slot into the daemon's extractor
//! registry keyed by file extension.

use anyhow::Result;
use std::path::Path;

/// Convert a file on disk into plain UTF-8 text suitable for chunking.
///
/// Implementations should be **fast** for non-applicable inputs:
/// return `Ok(None)` quickly rather than reading the file just to
/// reject it. The daemon iterates extractors per file and picks the
/// first that returns `Some`.
pub trait Extractor: Send + Sync {
    /// `None` means "not my type". `Err` means "would have been mine
    /// but extraction failed" — the daemon logs and skips.
    fn extract(&self, path: &Path) -> Result<Option<String>>;
}

/// Reads UTF-8 text files: .txt / .md / .rst / source code.
///
/// Anything binary will fail UTF-8 validation and we return `Ok(None)`
/// rather than burning cycles trying lossy decode.
pub struct TextExtractor;

impl TextExtractor {
    const TEXT_EXTS: &'static [&'static str] = &[
        "txt", "md", "markdown", "rst", "log",
        "py", "rs", "js", "ts", "tsx", "jsx", "go", "java", "c", "h",
        "cpp", "hpp", "cc", "cs", "rb", "php", "swift", "kt", "scala",
        "sh", "bash", "zsh", "fish",
        "yaml", "yml", "toml", "json", "ini", "conf",
        "html", "xml", "css", "scss", "sass", "less",
        "tex", "bib", "org",
    ];
}

impl Extractor for TextExtractor {
    fn extract(&self, path: &Path) -> Result<Option<String>> {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return Ok(None);
        };
        if !Self::TEXT_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            return Ok(None);
        }
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(s)),
            // Not valid UTF-8 — treat as a not-mine signal.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

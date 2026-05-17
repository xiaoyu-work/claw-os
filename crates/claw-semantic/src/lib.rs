//! claw-semantic — local semantic-search daemon for ClawOS.
//!
//! # Architecture (Phase 1: scaffold)
//!
//! Recoll handles the *keyword* layer (Xapian + traditional TF-IDF inverted
//! index). It works well for "find documents containing the literal string
//! ‘Q3 forecast’", but is useless for "find my pitch deck for Sequoia" when
//! the file content doesn't actually mention the word "Sequoia".
//!
//! `claw-semantic` provides the missing semantic / embedding layer:
//!
//! * A long-running daemon ([`bin/daemon.rs`]) that:
//!   - Walks the configured topdirs at startup.
//!   - Subscribes to filesystem events (notify / inotify) and reflects
//!     create / modify / delete in real time.
//!   - For each indexable file, extracts text → splits into chunks →
//!     embeds with a local model → upserts into a vector store.
//!
//! * A CLI ([`bin/cli.rs`]) exposing `status` / `search` / `reindex`
//!   verbs. `apps/docs/main.py` will eventually call this CLI (plus
//!   `recollq`) and fuse results with Reciprocal Rank Fusion so a single
//!   `docs.search` AI verb returns hybrid keyword+semantic hits.
//!
//! # Pluggable traits
//!
//! Each layer is a trait, so we can iterate per-commit without rewriting
//! the daemon shell:
//!
//! | Trait            | Phase 1 impl       | Phase 2+ target               |
//! |------------------|--------------------|-------------------------------|
//! | [`Extractor`]    | [`TextExtractor`]  | pdf, docx, html, rtf, msg     |
//! | [`Embedder`]     | [`StubEmbedder`]   | `fastembed-rs` (BGE-small)    |
//! | [`VectorStore`]  | [`MemoryStore`]    | LanceDB / sqlite-vec on disk  |
//!
//! # Config
//!
//! Reads `$XDG_CONFIG_HOME/claw-semantic/config.toml` (or
//! `~/.config/claw-semantic/config.toml`) — see [`config::Config`].
//! Defaults mirror the Recoll user config so we index the same topdirs by
//! default: `~/Documents`, `~/Desktop`, `~/Downloads`.

pub mod chunk;
pub mod config;
pub mod embed;
pub mod extract;
pub mod store;
pub mod watch;

pub use config::Config;
pub use embed::{Embedder, StubEmbedder};
pub use extract::{Extractor, TextExtractor};
pub use store::{MemoryStore, SearchHit, VectorStore};

/// A document chunk waiting to be embedded.
///
/// We chunk before embedding because:
/// 1. Most embedding models have a 512-token context cap.
/// 2. Fine-grained chunks let us return precise snippets in search hits
///    instead of just "this 80-page PDF is relevant".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    /// Absolute path to the source file.
    pub path: String,
    /// 0-based ordinal within the file. Two chunks with the same `(path,
    /// chunk_id)` represent the same logical span across reindex cycles.
    pub chunk_id: u32,
    /// Plain UTF-8 text of this chunk, after extraction.
    pub text: String,
}

//! Pure embedding abstractions for claw-os.
//!
//! ## What lives here
//!
//! - [`Embedder`] trait + request/response types — the contract every
//!   embedding backend (cloud, local, mock) implements.
//! - [`SemanticStore`] — SQLite-backed `(namespace, key, text, vec)`
//!   store with cosine similarity search and model-stickiness check.
//! - [`VectorStore`] and [`MemoryStore`] — the filesystem document
//!   index contract and its compatibility JSON implementation.
//! - Chunking / extraction / filesystem-walk utilities used by the
//!   `claw-semantic-daemon` to index user documents.
//!
//! ## What does NOT live here
//!
//! - Concrete LLM / GenAI inference engines (`onnxruntime-genai`,
//!   `llama.cpp`, …). Those host LLMs too, not just embeddings, and
//!   stay in `core::model::engines` next to the engine package manager.
//! - Anything that reads `core::config` globals. The factory layer
//!   that wires global config into this crate's pure types stays in
//!   `core::model::tasks::embed`.
//!
//! ## Stability
//!
//! This is an internal claw-os workspace crate. Versions follow the
//! containing repo, not semver — call sites all live in this monorepo
//! and update in lock-step with API changes.

pub mod chunk;
pub mod document_store;
pub mod embed;
pub mod extract;
pub mod store;
pub mod walk;

pub use chunk::{chunks_for, Chunk};
pub use document_store::{DocumentStoreError, MemoryStore, SearchHit, StoreStats, VectorStore};
pub use embed::{
    EmbedError, EmbedRequest, EmbedResponse, EmbedUsage, Embedder, StubEmbedder, EMBED_DIM,
};
pub use extract::{Extractor, TextExtractor};
pub use store::{SemanticError, SemanticHit, SemanticRow, SemanticStore, MAX_EMBED_TEXT_CHARS};
pub use walk::{walk, FsEvent, Watcher};

//! Filesystem semantic-search daemon and CLI orchestration for Claw OS.
//!
//! Reusable embedding, extraction, chunking, walking, and storage contracts
//! are owned by [`claw_embed`]. Compatibility modules preserve the original
//! `claw_semantic::{chunk, embed, extract, store, watch}` import paths while
//! callers migrate to `claw_embed` directly.

pub mod config;

pub use claw_embed::{
    Chunk, DocumentStoreError, EmbedError, EmbedRequest, EmbedResponse, EmbedUsage, Embedder,
    Extractor, MemoryStore, SearchHit, StoreStats, StubEmbedder, TextExtractor, VectorStore,
    EMBED_DIM,
};
pub use config::Config;

pub mod chunk {
    pub use claw_embed::chunk::*;
}

pub mod embed {
    pub use claw_embed::embed::*;
}

pub mod extract {
    pub use claw_embed::extract::*;
}

pub mod store {
    pub use claw_embed::document_store::*;
}

pub mod watch {
    pub use claw_embed::walk::*;
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/lib.rs"));
}

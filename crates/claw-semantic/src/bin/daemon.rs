//! `claw-semantic-daemon` — long-running indexer.
//!
//! On startup, walks the configured topdirs, extracts text, chunks,
//! embeds (stubbed in Phase 1), and writes to the vector store.
//! Then subscribes to inotify and reflects each create/modify/delete.
//!
//! Run via systemd --user (see the systemd feature's claw-semantic.service).

use anyhow::{Context, Result};
use claw_semantic::{
    chunk::chunks_for, watch::FsEvent, Config, EmbedRequest, Embedder, Extractor, MemoryStore,
    StubEmbedder, TextExtractor, VectorStore,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claw_semantic=info".into()),
        )
        .init();

    let cfg = Config::load_or_default().context("loading config")?;
    info!(?cfg, "starting claw-semantic-daemon");

    let store: Arc<dyn VectorStore> = Arc::new(MemoryStore::open(MemoryStore::default_path())?);
    let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
    let extractors: Vec<Box<dyn Extractor>> = vec![Box::new(TextExtractor)];

    // Phase 1: full sweep on startup, single-threaded for simplicity.
    // Phase 2 will switch to a worker pool sized to num_cpus / 2.
    info!("initial sweep");
    let files = claw_semantic::watch::walk(&cfg.topdirs);
    info!(n = files.len(), "files found, indexing");
    for p in files {
        if let Err(e) = index_file(&p, &cfg, embedder.as_ref(), &extractors, store.as_ref()).await {
            warn!(path = %p.display(), error = %e, "index failed, continuing");
        }
    }
    let stats = store.stats()?;
    info!(?stats, "initial sweep done");

    // Phase 1: subscribe to live events. Real coalescing / debouncing
    // comes in Phase 2 (right now we redo the file on every Modify
    // notification, which can fire many times during a save).
    let watcher =
        claw_semantic::watch::Watcher::spawn(cfg.topdirs.clone()).context("starting fs watcher")?;
    info!("watching for changes");
    loop {
        let Some(ev) = watcher.recv_with_timeout(Duration::from_secs(60)) else {
            continue;
        };
        match ev {
            FsEvent::Upsert(p) => {
                if !p.is_file() {
                    continue;
                }
                if let Err(e) =
                    index_file(&p, &cfg, embedder.as_ref(), &extractors, store.as_ref()).await
                {
                    warn!(path = %p.display(), error = %e, "index failed");
                }
            }
            FsEvent::Delete(p) => {
                let s = p.to_string_lossy().to_string();
                if let Err(e) = store.delete_path(&s) {
                    warn!(path = %s, error = %e, "delete from store failed");
                }
            }
        }
    }
}

async fn index_file(
    path: &Path,
    cfg: &Config,
    embedder: &dyn Embedder,
    extractors: &[Box<dyn Extractor>],
    store: &dyn VectorStore,
) -> Result<()> {
    if cfg.is_skipped(path) {
        return Ok(());
    }
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if !meta.is_file() {
        return Ok(());
    }
    if meta.len() > (cfg.max_file_mb as u64) * 1024 * 1024 {
        return Ok(());
    }

    let text = extractors
        .iter()
        .find_map(|e| e.extract(path).transpose())
        .transpose()?;
    let Some(text) = text else { return Ok(()) };

    let abs = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();
    let chunks = chunks_for(&abs, &text, cfg.chunk_chars, cfg.chunk_overlap_chars);
    if chunks.is_empty() {
        return Ok(());
    }
    let inputs: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let response = embedder.embed(EmbedRequest { inputs }).await?;
    store.upsert(&abs, &chunks, &response.embeddings)?;
    Ok(())
}

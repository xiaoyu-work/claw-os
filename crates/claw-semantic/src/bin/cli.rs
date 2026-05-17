//! `claw-semantic` — CLI surface for the daemon's data.
//!
//! Subcommands:
//!   status        Show corpus size + config + store path.
//!   search QUERY  Top-K nearest chunks for QUERY (uses the same
//!                 embedder as the daemon).
//!   reindex       Trigger a one-shot full sweep without restarting
//!                 the daemon (Phase 2 will RPC to the daemon; Phase 1
//!                 just walks topdirs synchronously like the daemon
//!                 startup does).
//!
//! All output is JSON (stable, consumable by apps/docs/main.py).

use anyhow::{Context, Result};
use claw_semantic::{
    chunk::chunks_for, Config, Embedder, Extractor, MemoryStore, StubEmbedder, TextExtractor,
    VectorStore,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "claw-semantic", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Status,
    Search {
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        k: usize,
    },
    Reindex,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load_or_default()?;
    let store = MemoryStore::open(MemoryStore::default_path())?;
    let embedder = StubEmbedder;

    match cli.cmd {
        Cmd::Status => {
            let stats = store.stats()?;
            let out = serde_json::json!({
                "store_path": MemoryStore::default_path(),
                "config_path": Config::path(),
                "topdirs": cfg.topdirs,
                "n_paths": stats.n_paths,
                "n_chunks": stats.n_chunks,
                "dim": stats.dim,
                "embedder": "stub-sha256",
                "note": "phase 1 stub embedder — search results are not yet semantically meaningful",
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Search { query, k } => {
            let qvec = embedder.embed(std::slice::from_ref(&query))?.pop().unwrap_or_default();
            let hits = store.search(&qvec, k)?;
            let out = serde_json::json!({
                "query": query,
                "k": k,
                "hits": hits,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Reindex => {
            let extractors: Vec<Box<dyn Extractor>> = vec![Box::new(TextExtractor)];
            let files = claw_semantic::watch::walk(&cfg.topdirs);
            let mut indexed = 0usize;
            let mut skipped = 0usize;
            for p in &files {
                if cfg.is_skipped(p) {
                    skipped += 1;
                    continue;
                }
                let Ok(meta) = std::fs::metadata(p) else {
                    skipped += 1;
                    continue;
                };
                if meta.len() > (cfg.max_file_mb as u64) * 1024 * 1024 {
                    skipped += 1;
                    continue;
                }
                let text = extractors
                    .iter()
                    .find_map(|e| e.extract(p).transpose())
                    .transpose()
                    .with_context(|| format!("extracting {}", p.display()))?;
                let Some(text) = text else {
                    skipped += 1;
                    continue;
                };
                let abs = p
                    .canonicalize()
                    .unwrap_or_else(|_| p.clone())
                    .to_string_lossy()
                    .to_string();
                let chunks = chunks_for(&abs, &text, cfg.chunk_chars, cfg.chunk_overlap_chars);
                if chunks.is_empty() {
                    skipped += 1;
                    continue;
                }
                let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
                let vecs = embedder.embed(&texts)?;
                store.upsert(&abs, &chunks, &vecs)?;
                indexed += 1;
            }
            let stats = store.stats()?;
            let out = serde_json::json!({
                "files_scanned": files.len(),
                "files_indexed": indexed,
                "files_skipped": skipped,
                "store": stats,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
    }
    Ok(())
}

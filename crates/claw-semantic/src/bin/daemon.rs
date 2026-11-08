//! Existing user file indexer with real local model vectors and live updates.

use anyhow::{Context, Result};
use claw_embed::{
    local::LocalEmbedder,
    walk::{FsEvent, Watcher},
};
use claw_semantic::{indexer, Config};
use std::{collections::BTreeSet, time::Duration};
use tracing::{info, warn};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claw_semantic=info".into()),
        )
        .init();
    let mut config = Config::load_or_default()?;
    config.validate()?;
    let (chunk_chars, chunk_overlap) = indexer::chunk_parameters(&config);
    info!(
        configured_chunk_chars = config.chunk_chars,
        chunk_chars, chunk_overlap, "file chunks use the bounded local inference token budget"
    );
    let embedder = LocalEmbedder::default();
    let status = embedder
        .status()
        .await
        .map_err(anyhow::Error::msg)
        .context("file index local embedding availability")?;
    anyhow::ensure!(
        status.configured,
        "file embedding is not ready: {}",
        status
            .detail
            .as_deref()
            .unwrap_or("local model/runtime missing")
    );
    let store = indexer::open_store(&indexer::database_path()?)?;
    let sweep = |config: &Config| {
        claw_embed::walk::walk_filtered(&config.topdirs, 100_000, &|path| config.is_skipped(path))
    };
    let mut watcher = Watcher::spawn(config.topdirs.clone())?;
    let files = sweep(&config)?;
    let mut initial_errors = 0;
    for path in &files {
        if let Err(error) = indexer::index_file(path, &config, &embedder, &store).await {
            initial_errors += 1;
            warn!(path=%path.display(),%error,"file not indexed");
        }
    }
    indexer::reconcile_files(&files, &store)?;
    info!(corpus=%indexer::corpus_stats(&store)?,error_count=initial_errors,"real local file index initial sweep completed");
    loop {
        let mut upserts = BTreeSet::new();
        let mut deletes = BTreeSet::new();
        let first = watcher.recv_with_timeout(Duration::from_secs(15));
        if let Some(event) = first {
            collect(event, &mut upserts, &mut deletes)?;
            while let Ok(event) = watcher.rx.try_recv() {
                collect(event, &mut upserts, &mut deletes)?;
                anyhow::ensure!(
                    upserts.len() + deletes.len() <= 100_000,
                    "file watcher batch exceeds its bound"
                );
            }
        }
        let next = Config::load_or_default()?;
        next.validate()?;
        let available_roots: Vec<_> = next
            .topdirs
            .iter()
            .filter(|path| path.is_dir())
            .cloned()
            .collect();
        if next.topdirs != config.topdirs
            || next.skip_patterns != config.skip_patterns
            || next.chunk_chars != config.chunk_chars
            || next.chunk_overlap_chars != config.chunk_overlap_chars
            || next.max_file_mb != config.max_file_mb
            || available_roots != watcher.topdirs
        {
            watcher = Watcher::spawn(next.topdirs.clone())?;
            config = next;
            let files = sweep(&config)?;
            indexer::reconcile_files(&files, &store)?;
            upserts.extend(files);
        }
        for path in deletes {
            indexer::remove_file(&path, &store)?;
        }
        for path in upserts {
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    for file in claw_embed::walk::walk_filtered(&[path], 100_000, &|path| {
                        config.is_skipped(path)
                    })? {
                        if let Err(error) =
                            indexer::index_file(&file, &config, &embedder, &store).await
                        {
                            warn!(path=%file.display(),%error,"updated file not indexed");
                        }
                    }
                }
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    if let Err(error) = indexer::index_file(&path, &config, &embedder, &store).await
                    {
                        warn!(path=%path.display(),%error,"updated file not indexed");
                    }
                }
                Ok(_) => indexer::remove_file(&path, &store)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    indexer::remove_file(&path, &store)?
                }
                Err(error) => return Err(error).context("inspect changed index source"),
            }
        }
    }
}

fn collect(
    event: FsEvent,
    upserts: &mut BTreeSet<std::path::PathBuf>,
    deletes: &mut BTreeSet<std::path::PathBuf>,
) -> Result<()> {
    match event {
        FsEvent::Upsert(path) => {
            deletes.remove(&path);
            upserts.insert(path);
        }
        FsEvent::Delete(path) => {
            upserts.remove(&path);
            deletes.insert(path);
        }
        FsEvent::Error(message) => {
            anyhow::bail!("file watcher failed; indexer restart required: {message}")
        }
    }
    Ok(())
}

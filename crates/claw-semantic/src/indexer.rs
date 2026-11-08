//! Real, revision-bound file vectors on the existing SemanticStore engine.

use anyhow::{Context, Result};
use claw_embed::{EmbedRequest, EmbedResponse, EmbedUsage, Embedder, SemanticStore, TextExtractor};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::Config;

pub const MAX_CHUNKS_PER_FILE: usize = 8192;
pub const MAX_INDEX_CHARS: usize = 8 * 1024 * 1024;

pub fn chunk_parameters(config: &Config) -> (usize, usize) {
    let size = config
        .chunk_chars
        .min(claw_embed::local::MAX_FILE_CHUNK_CHARS);
    (size, config.chunk_overlap_chars.min(size / 4))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOutcome {
    Indexed,
    Current,
    Unsupported,
    Excluded,
}

pub fn database_path() -> Result<PathBuf> {
    let state = match std::env::var_os("XDG_STATE_HOME") {
        Some(value) => PathBuf::from(value),
        None => dirs::home_dir()
            .context("file index owner home is unavailable")?
            .join(".local/state"),
    };
    anyhow::ensure!(
        state.is_absolute(),
        "file index state directory must be absolute"
    );
    Ok(state.join("claw-semantic").join("semantic.db"))
}

pub fn open_store(path: &Path) -> Result<SemanticStore> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .context("file index database has no directory")?;
    std::fs::create_dir_all(parent)?;
    let metadata = std::fs::symlink_metadata(parent)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "file index directory is unsafe"
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.nlink() == 1,
            "file index database is unsafe"
        );
    }
    let store = SemanticStore::open(path, None)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(store)
}

pub fn namespace(path: &Path) -> Result<String> {
    let path = path.to_str().context("indexed file path must be UTF-8")?;
    anyhow::ensure!(
        path.starts_with('/') && path.len() <= 4096,
        "invalid indexed file path"
    );
    Ok(format!("file/{path}"))
}

pub fn content_revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

pub async fn index_file(
    path: &Path,
    config: &Config,
    embedder: &dyn Embedder,
    store: &SemanticStore,
) -> Result<IndexOutcome> {
    let ns = namespace(path)?;
    if config.is_skipped(path) || !config.topdirs.iter().any(|root| path.starts_with(root)) {
        store.clear_namespace(&ns)?;
        return Ok(IndexOutcome::Excluded);
    }
    if !TextExtractor::supports(path) {
        store.clear_namespace(&ns)?;
        return Ok(IndexOutcome::Unsupported);
    }
    let mut file = claw_embed::file_source::source(path)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("open indexed source {}", path.display()))?;
    let before = file.metadata()?;
    anyhow::ensure!(
        before.is_file() && before.nlink() == 1,
        "index only regular single-link files"
    );
    let maximum = u64::from(config.max_file_mb) * 1024 * 1024;
    anyhow::ensure!(
        before.len() <= maximum,
        "indexed source exceeds its file-size limit"
    );
    let mut bytes = Vec::new();
    (&mut file).take(maximum + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= maximum && same_file(&before, &file.metadata()?),
        "indexed file changed while being read"
    );
    let revision = content_revision(&bytes);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            store.clear_namespace(&ns)?;
            return Ok(IndexOutcome::Unsupported);
        }
    };
    anyhow::ensure!(
        text.chars().count() <= MAX_INDEX_CHARS,
        "indexed text exceeds its character limit"
    );
    let (chunk_chars, chunk_overlap) = chunk_parameters(config);
    let chunks = claw_embed::chunks_for(
        path.to_str().context("file path must be UTF-8")?,
        &text,
        chunk_chars,
        chunk_overlap,
    );
    if chunks.is_empty() {
        store.clear_namespace(&ns)?;
        return Ok(IndexOutcome::Unsupported);
    }
    anyhow::ensure!(
        chunks.len() <= MAX_CHUNKS_PER_FILE,
        "file exceeds its chunk limit"
    );
    let first_key = format!("{revision}:0");
    if let Some(row) = store.get(&ns, &first_key)? {
        if row.model == embedder.model() && store.count(Some(&ns))? == chunks.len() as i64 {
            return Ok(IndexOutcome::Current);
        }
    }
    let mut merged = EmbedResponse {
        embeddings: Vec::with_capacity(chunks.len()),
        model: embedder.model().into(),
        dim: 0,
        usage: EmbedUsage::default(),
    };
    for batch in chunks.chunks(claw_embed::local::MAX_INPUTS) {
        let response = embedder
            .embed(EmbedRequest {
                inputs: batch.iter().map(|chunk| chunk.text.clone()).collect(),
            })
            .await?;
        anyhow::ensure!(
            response.model == merged.model
                && response.embeddings.len() == batch.len()
                && (merged.dim == 0 || merged.dim == response.dim),
            "embedding batch identity mismatch"
        );
        merged.dim = response.dim;
        merged.usage.prompt_tokens = merged
            .usage
            .prompt_tokens
            .saturating_add(response.usage.prompt_tokens);
        merged.usage.total_tokens = merged
            .usage
            .total_tokens
            .saturating_add(response.usage.total_tokens);
        merged.embeddings.extend(response.embeddings);
    }
    let current = claw_embed::file_source::source(path).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        same_file(&before, &file.metadata()?) && same_file(&before, &current.metadata()?),
        "file changed while embedding; index was not published"
    );
    let records: Vec<_> = chunks
        .into_iter()
        .map(|chunk| (format!("{revision}:{}", chunk.chunk_id), chunk.text))
        .collect();
    store.replace_namespace(&ns, &records, merged)?;
    Ok(IndexOutcome::Indexed)
}

pub fn remove_file(path: &Path, store: &SemanticStore) -> Result<()> {
    let prefix = namespace(path)?;
    for ns in store.namespaces()? {
        if ns == prefix
            || ns
                .strip_prefix(&prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            store.clear_namespace(&ns)?;
        }
    }
    Ok(())
}

pub fn reconcile_files(files: &[PathBuf], store: &SemanticStore) -> Result<()> {
    let existing: std::collections::BTreeSet<_> = files
        .iter()
        .map(|path| namespace(path))
        .collect::<Result<_, _>>()?;
    for ns in store.namespaces()? {
        if ns.starts_with("file/") && !existing.contains(&ns) {
            store.clear_namespace(&ns)?;
        }
    }
    Ok(())
}

pub fn corpus_stats(store: &SemanticStore) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "n_paths":store.namespaces()?.len(),
        "n_chunks":store.count(None)?,
        "model":store.pinned_model()?,
        "legacy_stub_index_used":false,
    }))
}

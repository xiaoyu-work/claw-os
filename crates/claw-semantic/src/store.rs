//! Vector store interface.
//!
//! Phase 1 ships [`MemoryStore`] — vectors in a `Vec`, persisted as
//! one JSON blob at `~/.local/state/claw-semantic/store.json`.
//! Search is naive cosine-similarity over every row. Fine up to maybe
//! 10k chunks; falls over above that.
//!
//! Phase 2 will replace this with a LanceDB-backed implementation
//! living in the same `~/.local/state/claw-semantic/` directory. The
//! trait below is intentionally narrow (upsert / delete by path /
//! search by vector) so swapping is a contained change.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub chunk_id: u32,
    pub snippet: String,
    pub score: f32,
}

pub trait VectorStore: Send + Sync {
    /// Replace every existing chunk for `path` with `vecs[i]` + the
    /// matching `chunks[i]`. Atomic per call: a partial write is not
    /// observable by `search`.
    fn upsert(
        &self,
        path: &str,
        chunks: &[crate::Chunk],
        vecs: &[Vec<f32>],
    ) -> Result<()>;

    /// Drop every chunk belonging to `path` (called on file deletion).
    fn delete_path(&self, path: &str) -> Result<()>;

    /// Top-K nearest neighbours by cosine similarity. Implementations
    /// must filter out chunks whose source file no longer exists.
    fn search(&self, qvec: &[f32], k: usize) -> Result<Vec<SearchHit>>;

    /// (path → number of indexed chunks) — used by `claw-semantic
    /// status` to give the user a sense of corpus size.
    fn stats(&self) -> Result<StoreStats>;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoreStats {
    pub n_paths: usize,
    pub n_chunks: usize,
    pub dim: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Row {
    path: String,
    chunk_id: u32,
    text: String,
    vec: Vec<f32>,
}

/// In-memory store backed by a single JSON file on disk. Concurrency-
/// safe via an `RwLock`. Persistence happens on every `upsert` /
/// `delete_path`, so the daemon survives crashes / restarts.
pub struct MemoryStore {
    inner: RwLock<Inner>,
    file: PathBuf,
}

#[derive(Default)]
struct Inner {
    /// Flat rows. Searches iterate this directly.
    rows: Vec<Row>,
    /// path → indices in `rows`. Speeds up upsert/delete.
    by_path: HashMap<String, Vec<usize>>,
}

impl MemoryStore {
    /// Resolve the default on-disk location:
    /// `$XDG_STATE_HOME/claw-semantic/store.json`, fallback
    /// `~/.local/state/claw-semantic/store.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
            return PathBuf::from(xdg)
                .join("claw-semantic")
                .join("store.json");
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.join(".local")
            .join("state")
            .join("claw-semantic")
            .join("store.json")
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let rows: Vec<Row> = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            if raw.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&raw)
                    .with_context(|| format!("parsing {}", path.display()))?
            }
        } else {
            Vec::new()
        };
        let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            by_path.entry(r.path.clone()).or_default().push(i);
        }
        Ok(Self {
            inner: RwLock::new(Inner { rows, by_path }),
            file: path,
        })
    }

    fn persist(&self, inner: &Inner) -> Result<()> {
        let tmp = self.file.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&inner.rows)?;
        std::fs::write(&tmp, &bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.file)
            .with_context(|| format!("renaming into {}", self.file.display()))?;
        Ok(())
    }
}

impl VectorStore for MemoryStore {
    fn upsert(
        &self,
        path: &str,
        chunks: &[crate::Chunk],
        vecs: &[Vec<f32>],
    ) -> Result<()> {
        if chunks.len() != vecs.len() {
            anyhow::bail!(
                "upsert: chunks ({}) != vecs ({})",
                chunks.len(),
                vecs.len()
            );
        }
        let mut inner = self.inner.write().unwrap();
        // Drop every existing row for path.
        if let Some(idxs) = inner.by_path.remove(path) {
            let mut to_remove = idxs;
            to_remove.sort_by(|a, b| b.cmp(a));
            for i in to_remove {
                inner.rows.swap_remove(i);
            }
            // by_path indices for *other* paths are now stale. Rebuild.
            inner.by_path.clear();
            let pairs: Vec<(String, usize)> = inner
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r.path.clone(), i))
                .collect();
            for (p, i) in pairs {
                inner.by_path.entry(p).or_default().push(i);
            }
        }
        // Append fresh rows.
        for (c, v) in chunks.iter().zip(vecs.iter()) {
            let i = inner.rows.len();
            inner.rows.push(Row {
                path: c.path.clone(),
                chunk_id: c.chunk_id,
                text: c.text.clone(),
                vec: v.clone(),
            });
            inner.by_path.entry(c.path.clone()).or_default().push(i);
        }
        self.persist(&inner)?;
        Ok(())
    }

    fn delete_path(&self, path: &str) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        if let Some(idxs) = inner.by_path.remove(path) {
            let mut to_remove = idxs;
            to_remove.sort_by(|a, b| b.cmp(a));
            for i in to_remove {
                inner.rows.swap_remove(i);
            }
            inner.by_path.clear();
            let pairs: Vec<(String, usize)> = inner
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r.path.clone(), i))
                .collect();
            for (p, i) in pairs {
                inner.by_path.entry(p).or_default().push(i);
            }
            self.persist(&inner)?;
        }
        Ok(())
    }

    fn search(&self, qvec: &[f32], k: usize) -> Result<Vec<SearchHit>> {
        let inner = self.inner.read().unwrap();
        let mut scored: Vec<(f32, &Row)> = inner
            .rows
            .iter()
            .filter(|r| Path::new(&r.path).exists())
            .map(|r| (cosine(qvec, &r.vec), r))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let hits = scored
            .into_iter()
            .take(k)
            .map(|(s, r)| SearchHit {
                path: r.path.clone(),
                chunk_id: r.chunk_id,
                snippet: snippet(&r.text, 240),
                score: s,
            })
            .collect();
        Ok(hits)
    }

    fn stats(&self) -> Result<StoreStats> {
        let inner = self.inner.read().unwrap();
        let dim = inner.rows.first().map(|r| r.vec.len()).unwrap_or(0);
        Ok(StoreStats {
            n_paths: inner.by_path.len(),
            n_chunks: inner.rows.len(),
            dim,
        })
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-12);
    dot / denom
}

fn snippet(text: &str, max: usize) -> String {
    let trimmed: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/store.rs"
    ));
}

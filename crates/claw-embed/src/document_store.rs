//! Filesystem document-vector storage contract and compatibility store.
//!
//! [`MemoryStore`] keeps the original `claw-semantic` JSON wire format and
//! default path so existing indexes remain readable after the contract moved
//! into this reusable primitives crate.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::Chunk;

#[derive(Debug, thiserror::Error)]
pub enum DocumentStoreError {
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("serializing document store {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("document store {operation} lock poisoned")]
    LockPoisoned { operation: &'static str },

    #[error("upsert chunks ({chunks}) != vectors ({vectors})")]
    LengthMismatch { chunks: usize, vectors: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub chunk_id: u32,
    pub snippet: String,
    pub score: f32,
}

pub trait VectorStore: Send + Sync {
    fn upsert(
        &self,
        path: &str,
        chunks: &[Chunk],
        vectors: &[Vec<f32>],
    ) -> Result<(), DocumentStoreError>;

    fn delete_path(&self, path: &str) -> Result<(), DocumentStoreError>;

    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<SearchHit>, DocumentStoreError>;

    fn stats(&self) -> Result<StoreStats, DocumentStoreError>;
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

/// In-memory document-vector store persisted as one JSON array.
pub struct MemoryStore {
    inner: RwLock<Inner>,
    file: PathBuf,
}

#[derive(Clone, Default)]
struct Inner {
    rows: Vec<Row>,
    by_path: HashMap<String, Vec<usize>>,
}

impl MemoryStore {
    /// The legacy path remains authoritative for compatibility:
    /// `$XDG_STATE_HOME/claw-semantic/store.json`, or
    /// `~/.local/state/claw-semantic/store.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
            return PathBuf::from(xdg).join("claw-semantic").join("store.json");
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.join(".local")
            .join("state")
            .join("claw-semantic")
            .join("store.json")
    }

    pub fn open(path: PathBuf) -> Result<Self, DocumentStoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| DocumentStoreError::Io {
                action: "creating",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rows = if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|source| DocumentStoreError::Io {
                action: "reading",
                path: path.clone(),
                source,
            })?;
            if raw.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&raw).map_err(|source| DocumentStoreError::Parse {
                    path: path.clone(),
                    source,
                })?
            }
        } else {
            Vec::new()
        };
        let by_path = build_path_index(&rows);
        Ok(Self {
            inner: RwLock::new(Inner { rows, by_path }),
            file: path,
        })
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, Inner>, DocumentStoreError> {
        self.inner
            .read()
            .map_err(|_| DocumentStoreError::LockPoisoned { operation: "read" })
    }

    fn write(&self) -> Result<RwLockWriteGuard<'_, Inner>, DocumentStoreError> {
        self.inner
            .write()
            .map_err(|_| DocumentStoreError::LockPoisoned { operation: "write" })
    }

    fn persist(&self, inner: &Inner) -> Result<(), DocumentStoreError> {
        let temporary = self.file.with_extension("json.tmp");
        let bytes =
            serde_json::to_vec(&inner.rows).map_err(|source| DocumentStoreError::Serialize {
                path: self.file.clone(),
                source,
            })?;
        std::fs::write(&temporary, bytes).map_err(|source| DocumentStoreError::Io {
            action: "writing",
            path: temporary.clone(),
            source,
        })?;
        std::fs::rename(&temporary, &self.file).map_err(|source| DocumentStoreError::Io {
            action: "renaming into",
            path: self.file.clone(),
            source,
        })
    }
}

impl VectorStore for MemoryStore {
    fn upsert(
        &self,
        path: &str,
        chunks: &[Chunk],
        vectors: &[Vec<f32>],
    ) -> Result<(), DocumentStoreError> {
        if chunks.len() != vectors.len() {
            return Err(DocumentStoreError::LengthMismatch {
                chunks: chunks.len(),
                vectors: vectors.len(),
            });
        }
        let mut inner = self.write()?;
        let mut next = inner.clone();
        if let Some(indices) = next.by_path.remove(path) {
            remove_rows(&mut next.rows, indices);
            next.by_path = build_path_index(&next.rows);
        }
        for (chunk, vector) in chunks.iter().zip(vectors) {
            let index = next.rows.len();
            next.rows.push(Row {
                path: chunk.path.clone(),
                chunk_id: chunk.chunk_id,
                text: chunk.text.clone(),
                vec: vector.clone(),
            });
            next.by_path
                .entry(chunk.path.clone())
                .or_default()
                .push(index);
        }
        self.persist(&next)?;
        *inner = next;
        Ok(())
    }

    fn delete_path(&self, path: &str) -> Result<(), DocumentStoreError> {
        let mut inner = self.write()?;
        let mut next = inner.clone();
        if let Some(indices) = next.by_path.remove(path) {
            remove_rows(&mut next.rows, indices);
            next.by_path = build_path_index(&next.rows);
            self.persist(&next)?;
            *inner = next;
        }
        Ok(())
    }

    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<SearchHit>, DocumentStoreError> {
        let inner = self.read()?;
        let mut scored: Vec<(f32, &Row)> = inner
            .rows
            .iter()
            .filter(|row| Path::new(&row.path).exists())
            .map(|row| (cosine(query, &row.vec), row))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(score, row)| SearchHit {
                path: row.path.clone(),
                chunk_id: row.chunk_id,
                snippet: snippet(&row.text, 240),
                score,
            })
            .collect())
    }

    fn stats(&self) -> Result<StoreStats, DocumentStoreError> {
        let inner = self.read()?;
        Ok(StoreStats {
            n_paths: inner.by_path.len(),
            n_chunks: inner.rows.len(),
            dim: inner.rows.first().map(|row| row.vec.len()).unwrap_or(0),
        })
    }
}

fn build_path_index(rows: &[Row]) -> HashMap<String, Vec<usize>> {
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        by_path.entry(row.path.clone()).or_default().push(index);
    }
    by_path
}

fn remove_rows(rows: &mut Vec<Row>, mut indices: Vec<usize>) {
    indices.sort_unstable_by(|a, b| b.cmp(a));
    for index in indices {
        rows.swap_remove(index);
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (a, b) in a.iter().zip(b) {
        dot += a * b;
        norm_a += a * a;
        norm_b += b * b;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt()).max(1e-12)
}

fn snippet(text: &str, max_chars: usize) -> String {
    let trimmed: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/document_store.rs"
    ));
}

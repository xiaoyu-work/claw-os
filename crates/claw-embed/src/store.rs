//! Semantic memory backed by an [`Embedder`] (cloud or local).
//!
//! Stores `(namespace, key, text, embedding)` rows in a SQLite
//! database. Queries embed the input through the same provider and
//! rank rows by cosine similarity over the unit-normalised vectors.
//!
//! - **Backend-agnostic.** Any `Box<dyn Embedder>` works — Azure
//!   OpenAI, local `onnxruntime-genai` (Qwen3), mock test embedders.
//!   The store doesn't care which.
//! - **Brute-force ranking.** O(rows × dim) per query in pure Rust.
//!   At 1024 dim and ~10K rows that's ~80ms — acceptable for an agent
//!   memory of conversation messages and personal-doc indexes. Swap
//!   in `sqlite-vec` / `usearch` later if the corpus grows past that.
//! - **Re-index by key.** `(namespace, key)` is the upsert primary
//!   key — re-embedding a message overwrites the previous vector
//!   cleanly.
//!
//! Failures are non-fatal at call sites: a missing or misconfigured
//! embedder returns `Disabled`, transport errors bubble up, and
//! callers should log + continue rather than crash the agent loop.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::embed::{EmbedRequest, Embedder};

#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("semantic db poisoned: {0}")]
    Poisoned(String),

    #[error("embedder: {0}")]
    Embed(String),

    #[error("semantic store disabled (no embedder configured)")]
    Disabled,

    #[error("dimension mismatch: row has {row}, query has {query}")]
    DimMismatch { row: usize, query: usize },

    #[error("model mismatch: store is pinned to `{existing}`, refused vector from `{incoming}`. Embedding models cannot be mixed in one corpus — clear the store or use a separate db.")]
    ModelMismatch { existing: String, incoming: String },
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS semantic_docs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    namespace   TEXT NOT NULL,
    key         TEXT NOT NULL,
    text        TEXT NOT NULL,
    model       TEXT NOT NULL,
    dim         INTEGER NOT NULL,
    embedding   BLOB NOT NULL,
    ts_ms       INTEGER NOT NULL,
    UNIQUE(namespace, key)
);

CREATE INDEX IF NOT EXISTS semantic_docs_ns_ts
    ON semantic_docs(namespace, ts_ms);
"#;

#[derive(Debug, Clone)]
pub struct SemanticHit {
    pub id: i64,
    pub namespace: String,
    pub key: String,
    pub text: String,
    pub model: String,
    pub score: f32,
    pub ts_ms: i64,
}

/// On-disk row, exposed for callers that need raw access.
#[derive(Debug, Clone)]
pub struct SemanticRow {
    pub id: i64,
    pub namespace: String,
    pub key: String,
    pub text: String,
    pub model: String,
    pub dim: usize,
    pub embedding: Vec<f32>,
    pub ts_ms: i64,
}

#[derive(Clone)]
pub struct SemanticStore {
    conn: Arc<Mutex<Connection>>,
    embedder: Option<Arc<dyn Embedder>>,
}

impl SemanticStore {
    /// Open (or create) the store at `path`, attaching `embedder` for
    /// `index`/`search` calls. Pass `None` to open in *query-only*
    /// mode (e.g. for tooling that just enumerates rows).
    pub fn open(
        path: impl AsRef<Path>,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, SemanticError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder,
        })
    }

    /// Open an in-memory store — used for tests and ephemeral sessions.
    pub fn open_in_memory(embedder: Option<Arc<dyn Embedder>>) -> Result<Self, SemanticError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder,
        })
    }

    pub fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        self.embedder.clone()
    }

    /// Attach (or replace) the embedder. Useful in tests and CLI tools
    /// that want to swap providers at runtime.
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Embed `text` and upsert under `(namespace, key)`. Calls the
    /// configured embedder once; the resulting vector is L2-normalised
    /// in place so cosine similarity reduces to a dot product at query
    /// time.
    ///
    /// **Stickiness guard.** Once any row exists, subsequent indexes
    /// must come from the same model — switching embedders mid-corpus
    /// produces incompatible vector spaces and silently broken
    /// search. Mismatches return `SemanticError::ModelMismatch` so
    /// callers can either keep the old model or wipe the store.
    pub async fn index(
        &self,
        namespace: &str,
        key: &str,
        text: &str,
    ) -> Result<i64, SemanticError> {
        let embedder = self.embedder.as_ref().ok_or(SemanticError::Disabled)?;
        // Bound the input we send to the embedder; some providers
        // accept inputs > 8 KiB but charge per token and stall on
        // multi-megabyte payloads. Truncating at char boundary
        // protects against panics on multi-byte UTF-8.
        let bounded = truncate_to_chars(text, MAX_EMBED_TEXT_CHARS);
        let resp = embedder
            .embed(EmbedRequest {
                inputs: vec![bounded],
            })
            .await
            .map_err(|e| SemanticError::Embed(e.to_string()))?;
        let mut vec = resp
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| SemanticError::Embed("provider returned 0 embeddings".into()))?;
        normalise(&mut vec);
        let dim = vec.len();
        let blob = encode_vec(&vec);
        let ts = current_ts_ms();
        // Wrap the stickiness check + insert in a transaction so a
        // concurrent indexer can't squeak in a row of a different
        // model between the SELECT and the UPSERT.
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row("SELECT model FROM semantic_docs LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        if let Some(existing) = existing {
            if existing != resp.model {
                return Err(SemanticError::ModelMismatch {
                    existing,
                    incoming: resp.model,
                });
            }
        }
        tx.execute(
            "INSERT INTO semantic_docs (namespace, key, text, model, dim, embedding, ts_ms)
                VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(namespace, key) DO UPDATE SET
                text=excluded.text,
                model=excluded.model,
                dim=excluded.dim,
                embedding=excluded.embedding,
                ts_ms=excluded.ts_ms",
            params![namespace, key, text, resp.model, dim as i64, blob, ts],
        )?;
        let rowid = tx.last_insert_rowid();
        tx.commit()?;
        Ok(rowid)
    }

    /// Embed `query` and rank rows (optionally filtered by `namespace`)
    /// by cosine similarity. Returns the top `limit` hits, highest
    /// score first.
    pub async fn search(
        &self,
        namespace: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SemanticHit>, SemanticError> {
        let embedder = self.embedder.as_ref().ok_or(SemanticError::Disabled)?;
        let resp = embedder
            .embed(EmbedRequest {
                inputs: vec![query.to_string()],
            })
            .await
            .map_err(|e| SemanticError::Embed(e.to_string()))?;
        let mut q = resp
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| SemanticError::Embed("provider returned 0 embeddings".into()))?;
        normalise(&mut q);
        self.search_with_vector(namespace, &q, limit)
    }

    /// Rank rows against an already-computed (and ideally normalised)
    /// query vector — cheaper when the same query is reused across
    /// namespaces, and lets callers re-use embeddings they computed
    /// elsewhere.
    pub fn search_with_vector(
        &self,
        namespace: Option<&str>,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<SemanticHit>, SemanticError> {
        // We collect the candidate rows under the mutex into a small
        // intermediate buffer, then *drop the lock* before doing the
        // O(rows × dim) scoring work. Holding the connection mutex
        // across a busy CPU loop was starving other writers (notably
        // the FTS recorder) on long-corpus queries.
        let candidates: Vec<(SemanticHit, Vec<u8>, usize)> = {
            let conn = self.lock_conn()?;
            let sql = if namespace.is_some() {
                "SELECT id, namespace, key, text, model, dim, embedding, ts_ms
                    FROM semantic_docs WHERE namespace = ?"
            } else {
                "SELECT id, namespace, key, text, model, dim, embedding, ts_ms
                    FROM semantic_docs"
            };
            let mut stmt = conn.prepare(sql)?;
            let mut rows = if let Some(ns) = namespace {
                stmt.query(params![ns])?
            } else {
                stmt.query([])?
            };
            let mut buf: Vec<(SemanticHit, Vec<u8>, usize)> = Vec::new();
            while let Some(row) = rows.next()? {
                let dim: i64 = row.get(5)?;
                if (dim as usize) != query.len() {
                    // Skip rows from a different model / dim — we may
                    // have a mixed corpus during a model upgrade.
                    continue;
                }
                let blob: Vec<u8> = row.get(6)?;
                let hit = SemanticHit {
                    id: row.get(0)?,
                    namespace: row.get(1)?,
                    key: row.get(2)?,
                    text: row.get(3)?,
                    model: row.get(4)?,
                    score: 0.0,
                    ts_ms: row.get(7)?,
                };
                buf.push((hit, blob, dim as usize));
            }
            buf
        }; // mutex released here

        let mut hits: Vec<SemanticHit> = Vec::with_capacity(candidates.len());
        for (mut hit, blob, dim) in candidates {
            let v = match decode_vec(&blob, dim) {
                Ok(v) => v,
                Err(_) => {
                    // Corrupted row — skip rather than aborting the
                    // whole search. Surfacing the error in a hot path
                    // would degrade query stability on a single bad
                    // blob.
                    continue;
                }
            };
            hit.score = dot(&v, query)?;
            hits.push(hit);
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Number of rows matching the optional `namespace` filter.
    pub fn count(&self, namespace: Option<&str>) -> Result<i64, SemanticError> {
        let conn = self.lock_conn()?;
        let n: i64 = if let Some(ns) = namespace {
            conn.query_row(
                "SELECT COUNT(*) FROM semantic_docs WHERE namespace = ?",
                params![ns],
                |r| r.get(0),
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM semantic_docs", [], |r| r.get(0))?
        };
        Ok(n)
    }

    /// Delete a single row identified by `(namespace, key)`. Returns
    /// `Ok(true)` if a row existed.
    pub fn remove(&self, namespace: &str, key: &str) -> Result<bool, SemanticError> {
        let conn = self.lock_conn()?;
        let n = conn.execute(
            "DELETE FROM semantic_docs WHERE namespace = ? AND key = ?",
            params![namespace, key],
        )?;
        Ok(n > 0)
    }

    /// Drop every row in `namespace`. Returns the number deleted.
    pub fn clear_namespace(&self, namespace: &str) -> Result<usize, SemanticError> {
        let conn = self.lock_conn()?;
        let n = conn.execute(
            "DELETE FROM semantic_docs WHERE namespace = ?",
            params![namespace],
        )?;
        Ok(n)
    }

    /// Drop every row in the entire store. Use this when migrating to
    /// a different embedding model — vector spaces are not compatible
    /// across models, and the [`SemanticError::ModelMismatch`] check
    /// will refuse new vectors otherwise. Returns the number deleted.
    pub fn clear_all(&self) -> Result<usize, SemanticError> {
        let conn = self.lock_conn()?;
        let n = conn.execute("DELETE FROM semantic_docs", [])?;
        Ok(n)
    }

    /// Returns the model name currently pinned in the store (the model
    /// of any existing row), or `Ok(None)` if the store is empty.
    /// Useful for telling the user "your current corpus is on model X,
    /// switching means you'll need to re-index."
    pub fn pinned_model(&self) -> Result<Option<String>, SemanticError> {
        let conn = self.lock_conn()?;
        let m = conn
            .query_row("SELECT model FROM semantic_docs LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        Ok(m)
    }

    pub fn list(
        &self,
        namespace: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SemanticRow>, SemanticError> {
        let conn = self.lock_conn()?;
        let sql = if namespace.is_some() {
            "SELECT id, namespace, key, text, model, dim, embedding, ts_ms
                FROM semantic_docs WHERE namespace = ?
                ORDER BY ts_ms DESC LIMIT ?"
        } else {
            "SELECT id, namespace, key, text, model, dim, embedding, ts_ms
                FROM semantic_docs ORDER BY ts_ms DESC LIMIT ?"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut out = Vec::new();
        let mut rows = if let Some(ns) = namespace {
            stmt.query(params![ns, limit as i64])?
        } else {
            stmt.query(params![limit as i64])?
        };
        while let Some(row) = rows.next()? {
            let dim: i64 = row.get(5)?;
            let blob: Vec<u8> = row.get(6)?;
            let embedding = match decode_vec(&blob, dim as usize) {
                Ok(v) => v,
                Err(_) => continue,
            };
            out.push(SemanticRow {
                id: row.get(0)?,
                namespace: row.get(1)?,
                key: row.get(2)?,
                text: row.get(3)?,
                model: row.get(4)?,
                dim: dim as usize,
                embedding,
                ts_ms: row.get(7)?,
            });
        }
        Ok(out)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SemanticError> {
        self.conn
            .lock()
            .map_err(|e| SemanticError::Poisoned(e.to_string()))
    }
}

fn current_ts_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hard cap on the size of any one text body we'll embed. Anything
/// larger gets truncated to a character boundary so we don't
/// accidentally ship a multi-MB document to the embedder (most
/// providers charge per token and time-out on huge inputs).
pub const MAX_EMBED_TEXT_CHARS: usize = 8 * 1024;

fn truncate_to_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

fn encode_vec(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn decode_vec(blob: &[u8], dim: usize) -> Result<Vec<f32>, SemanticError> {
    // Each f32 is 4 bytes; the blob must be exactly `dim * 4` bytes,
    // not just "at least". A short blob means a corrupted row or a
    // mismatched-dim insert; either way we refuse to silently
    // pad-with-zeros and skew similarity scores.
    let expected = dim
        .checked_mul(4)
        .ok_or_else(|| SemanticError::Embed(format!("invalid dim {dim} (overflow)")))?;
    if blob.len() != expected {
        return Err(SemanticError::DimMismatch {
            row: blob.len() / 4,
            query: dim,
        });
    }
    let mut out = Vec::with_capacity(dim);
    for chunk in blob.chunks_exact(4) {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(chunk);
        out.push(f32::from_le_bytes(buf));
    }
    Ok(out)
}

fn dot(a: &[f32], b: &[f32]) -> Result<f32, SemanticError> {
    if a.len() != b.len() {
        return Err(SemanticError::DimMismatch {
            row: a.len(),
            query: b.len(),
        });
    }
    Ok(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum())
}

fn normalise(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/store.rs"
    ));
}

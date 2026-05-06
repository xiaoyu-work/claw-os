//! Semantic memory backed by an [`Embedder`] (cloud or local).
//!
//! Stores `(namespace, key, text, embedding)` rows in a sidecar SQLite
//! database (`data_dir/agent/semantic.db` by default). Queries embed
//! the input through the same provider and rank rows by cosine
//! similarity over the unit-normalised vectors.
//!
//! - **Backend-agnostic.** Any `Box<dyn Embedder>` works — Azure OpenAI
//!   via `[embed]` config today, fastembed-rs / local ONNX once those
//!   land. The store doesn't care which.
//! - **Brute-force ranking.** O(rows × dim) per query in pure Rust.
//!   At 1536 dim and ~10K rows that's ~60ms — acceptable for an agent
//!   memory of conversation messages and notes. Swap in `sqlite-vec` /
//!   `usearch` later if the corpus grows past that.
//! - **Re-index by key.** `(namespace, key)` is the upsert primary key
//!   — re-embedding a message overwrites the previous vector cleanly.
//!
//! Failures are non-fatal at call sites: a missing or misconfigured
//! embedder returns `Disabled`, transport errors bubble up, and
//! callers should log + continue rather than crash the agent loop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::model::tasks::embed::{EmbedRequest, Embedder};

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
    ModelMismatch {
        existing: String,
        incoming: String,
    },
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
    /// `index`/`search` calls. Pass `None` to open in *query-only* mode
    /// (e.g. for tooling that just enumerates rows).
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

    /// Build a store using the system memory path and the
    /// configured-default embedder. Returns `Ok(None)` if embedding is
    /// disabled in config; returns an error if the config block names
    /// an unknown provider or if the DB cannot be opened.
    pub fn open_default() -> Result<Option<Self>, SemanticError> {
        let embedder = match crate::model::tasks::embed::build_default() {
            Ok(Some(e)) => e,
            Ok(None) => return Ok(None),
            Err(e) => return Err(SemanticError::Embed(e)),
        };
        let path = default_path();
        let store = Self::open(path, Some(Arc::from(embedder)))?;
        Ok(Some(store))
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
        let resp = embedder
            .embed(EmbedRequest {
                inputs: vec![text.to_string()],
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
        let conn = self.lock_conn()?;
        // Stickiness: refuse to mix vector spaces in one corpus.
        let existing: Option<String> = conn
            .query_row(
                "SELECT model FROM semantic_docs LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != resp.model {
                return Err(SemanticError::ModelMismatch {
                    existing,
                    incoming: resp.model,
                });
            }
        }
        conn.execute(
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
        Ok(conn.last_insert_rowid())
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
        let conn = self.lock_conn()?;
        let mut hits: Vec<SemanticHit> = Vec::new();
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
        while let Some(row) = rows.next()? {
            let dim: i64 = row.get(5)?;
            let blob: Vec<u8> = row.get(6)?;
            if (dim as usize) != query.len() {
                // Skip rows from a different model / dim instead of
                // erroring — we may have a mixed corpus during a
                // model upgrade and want partial answers.
                continue;
            }
            let v = decode_vec(&blob, dim as usize);
            let score = dot(&v, query);
            hits.push(SemanticHit {
                id: row.get(0)?,
                namespace: row.get(1)?,
                key: row.get(2)?,
                text: row.get(3)?,
                model: row.get(4)?,
                score,
                ts_ms: row.get(7)?,
            });
        }
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
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

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SemanticError> {
        self.conn
            .lock()
            .map_err(|e| SemanticError::Poisoned(e.to_string()))
    }
}

/// Default path under the cos data dir: `<data_dir>/agent/semantic.db`.
pub fn default_path() -> PathBuf {
    crate::paths::agent_semantic_db_path()
}

fn current_ts_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn encode_vec(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn decode_vec(blob: &[u8], dim: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(dim);
    for chunk in blob.chunks_exact(4).take(dim) {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(chunk);
        out.push(f32::from_le_bytes(buf));
    }
    out
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
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
    use super::*;
    use crate::model::tasks::embed::{
        EmbedError, EmbedRequest, EmbedResponse, EmbedUsage, Embedder,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test embedder: returns a deterministic vector based on the
    /// hash of the input string. Designed to make orthogonal-ish
    /// vectors per input so similarity ordering is meaningful.
    struct HashEmbedder {
        dim: usize,
        calls: AtomicUsize,
    }

    impl HashEmbedder {
        fn new(dim: usize) -> Self {
            Self {
                dim,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Embedder for HashEmbedder {
        fn name(&self) -> &str {
            "hash-test"
        }
        fn model(&self) -> &str {
            "hash-test/v1"
        }
        fn is_configured(&self) -> bool {
            true
        }
        async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(request.inputs.len());
            for s in &request.inputs {
                // Stable per-string vector — hash bytes into the dim
                // slots, producing distinguishable outputs per input.
                let mut v = vec![0.0f32; self.dim];
                for (i, b) in s.bytes().enumerate() {
                    let slot = (i + b as usize) % self.dim;
                    v[slot] += (b as f32) * 0.01;
                }
                if v.iter().all(|x| *x == 0.0) {
                    v[0] = 1.0;
                }
                out.push(v);
            }
            Ok(EmbedResponse {
                embeddings: out,
                model: "hash-test/v1".to_string(),
                dim: self.dim,
                usage: EmbedUsage::default(),
            })
        }
    }

    fn store_with_hash(dim: usize) -> SemanticStore {
        let e: Arc<dyn Embedder> = Arc::new(HashEmbedder::new(dim));
        SemanticStore::open_in_memory(Some(e)).unwrap()
    }

    /// Variant that lets each test pin a custom model identifier.
    struct TaggedHashEmbedder {
        inner: HashEmbedder,
        model: String,
    }

    #[async_trait]
    impl Embedder for TaggedHashEmbedder {
        fn name(&self) -> &str {
            "tagged"
        }
        fn model(&self) -> &str {
            &self.model
        }
        fn is_configured(&self) -> bool {
            true
        }
        async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
            let mut resp = self.inner.embed(request).await?;
            resp.model = self.model.clone();
            Ok(resp)
        }
    }

    #[tokio::test]
    async fn index_and_search_roundtrip() {
        let s = store_with_hash(64);
        s.index("notes", "k1", "alpha beta gamma").await.unwrap();
        s.index("notes", "k2", "completely different content")
            .await
            .unwrap();
        s.index("notes", "k3", "alpha beta delta").await.unwrap();

        let hits = s.search(Some("notes"), "alpha beta", 2).await.unwrap();
        assert_eq!(hits.len(), 2);
        // Top hits should be the two strings sharing the "alpha beta" prefix.
        let keys: Vec<&str> = hits.iter().map(|h| h.key.as_str()).collect();
        assert!(keys.contains(&"k1") || keys.contains(&"k3"));
    }

    #[tokio::test]
    async fn index_upserts_on_same_key() {
        let s = store_with_hash(32);
        s.index("notes", "k1", "first").await.unwrap();
        s.index("notes", "k1", "second").await.unwrap();
        assert_eq!(s.count(Some("notes")).unwrap(), 1);
        let hits = s.search(Some("notes"), "second", 1).await.unwrap();
        assert_eq!(hits[0].text, "second");
    }

    #[tokio::test]
    async fn namespace_filter_is_respected() {
        let s = store_with_hash(32);
        s.index("ns_a", "k", "shared text").await.unwrap();
        s.index("ns_b", "k", "shared text").await.unwrap();
        let a = s.search(Some("ns_a"), "shared text", 5).await.unwrap();
        let b = s.search(Some("ns_b"), "shared text", 5).await.unwrap();
        let all = s.search(None, "shared text", 5).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(all.len(), 2);
        assert_eq!(a[0].namespace, "ns_a");
        assert_eq!(b[0].namespace, "ns_b");
    }

    #[tokio::test]
    async fn search_returns_top_k_in_score_order() {
        let s = store_with_hash(32);
        for i in 0..5 {
            s.index("docs", &format!("k{i}"), &format!("doc number {i}"))
                .await
                .unwrap();
        }
        let hits = s.search(Some("docs"), "doc number 2", 3).await.unwrap();
        assert_eq!(hits.len(), 3);
        for w in hits.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "hits not in descending score order: {} vs {}",
                w[0].score,
                w[1].score
            );
        }
    }

    #[test]
    fn search_with_vector_skips_dim_mismatched_rows() {
        // Insert two rows with different dims by hand and check
        // search_with_vector silently skips the mismatched one.
        let s: SemanticStore = SemanticStore::open_in_memory(None).unwrap();
        let conn = s.conn.lock().unwrap();
        let v8 = vec![0.5f32; 8];
        let v16 = vec![0.5f32; 16];
        conn.execute(
            "INSERT INTO semantic_docs (namespace, key, text, model, dim, embedding, ts_ms) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params!["x", "a", "tiny", "m1", 8i64, encode_vec(&v8), current_ts_ms()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO semantic_docs (namespace, key, text, model, dim, embedding, ts_ms) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params!["x", "b", "right", "m2", 16i64, encode_vec(&v16), current_ts_ms()],
        )
        .unwrap();
        drop(conn);
        let q = vec![0.5f32; 16];
        let hits = s.search_with_vector(Some("x"), &q, 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "b");
    }

    #[tokio::test]
    async fn remove_and_clear_namespace_work() {
        let s = store_with_hash(16);
        s.index("ns", "a", "x").await.unwrap();
        s.index("ns", "b", "y").await.unwrap();
        s.index("other", "c", "z").await.unwrap();
        assert!(s.remove("ns", "a").unwrap());
        assert!(!s.remove("ns", "a").unwrap()); // already gone
        assert_eq!(s.count(Some("ns")).unwrap(), 1);
        assert_eq!(s.clear_namespace("ns").unwrap(), 1);
        assert_eq!(s.count(Some("ns")).unwrap(), 0);
        assert_eq!(s.count(Some("other")).unwrap(), 1);
    }

    #[tokio::test]
    async fn search_without_embedder_errors_disabled() {
        let s: SemanticStore = SemanticStore::open_in_memory(None).unwrap();
        let err = s.search(None, "anything", 5).await.unwrap_err();
        assert!(matches!(err, SemanticError::Disabled));
    }

    #[test]
    fn encode_decode_roundtrips() {
        let v = vec![0.1f32, -0.5, 0.0, 1.5, f32::NEG_INFINITY, f32::INFINITY];
        let blob = encode_vec(&v);
        let back = decode_vec(&blob, v.len());
        assert_eq!(back, v);
    }

    #[test]
    fn normalise_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        normalise(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dot_of_normalised_orthogonal_is_zero() {
        let mut a = vec![1.0f32, 0.0];
        let mut b = vec![0.0f32, 1.0];
        normalise(&mut a);
        normalise(&mut b);
        assert!((dot(&a, &b)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn index_refuses_to_mix_embedding_models() {
        // First populate the store with model "model-a".
        let e1: Arc<dyn Embedder> = Arc::new(TaggedHashEmbedder {
            inner: HashEmbedder::new(64),
            model: "model-a".to_string(),
        });
        let s = SemanticStore::open_in_memory(Some(e1)).unwrap();
        s.index("notes", "k1", "alpha").await.unwrap();

        // Swap to a different model — index() must refuse, the
        // shared sqlite connection is preserved by `with_embedder`.
        let e2: Arc<dyn Embedder> = Arc::new(TaggedHashEmbedder {
            inner: HashEmbedder::new(64),
            model: "model-b".to_string(),
        });
        let s2 = s.with_embedder(e2);

        let err = s2.index("notes", "k2", "beta").await.unwrap_err();
        match err {
            SemanticError::ModelMismatch {
                existing,
                incoming,
            } => {
                assert_eq!(existing, "model-a");
                assert_eq!(incoming, "model-b");
            }
            other => panic!("expected ModelMismatch, got {other:?}"),
        }
    }
}

// Re-exported because callers occasionally want to inspect raw rows.
#[allow(dead_code)]
impl SemanticStore {
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
            out.push(SemanticRow {
                id: row.get(0)?,
                namespace: row.get(1)?,
                key: row.get(2)?,
                text: row.get(3)?,
                model: row.get(4)?,
                dim: dim as usize,
                embedding: decode_vec(&blob, dim as usize),
                ts_ms: row.get(7)?,
            });
        }
        Ok(out)
    }
}

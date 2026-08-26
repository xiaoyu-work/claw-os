use super::*;
use crate::embed::{EmbedError, EmbedRequest, EmbedResponse, EmbedUsage, Embedder};
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
    let back = decode_vec(&blob, v.len()).unwrap();
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
    assert!((dot(&a, &b).unwrap()).abs() < 1e-6);
}

#[test]
fn dim_mismatch_errors() {
    // Cross-dimension `dot` must surface an error so silent
    // truncation can't poison cosine scores.
    let err = dot(&[1.0, 0.0], &[1.0, 0.0, 0.0]).unwrap_err();
    assert!(matches!(
        err,
        SemanticError::DimMismatch { row: 2, query: 3 }
    ));

    // `decode_vec` of a truncated blob must also error rather
    // than silently zero-pad and skew similarity later.
    let v = vec![0.1f32, -0.5, 1.5];
    let blob = encode_vec(&v); // 12 bytes
    let too_few = &blob[..8]; // only 2 floats' worth
    let err = decode_vec(too_few, 3).unwrap_err();
    assert!(matches!(err, SemanticError::DimMismatch { .. }));
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
        SemanticError::ModelMismatch { existing, incoming } => {
            assert_eq!(existing, "model-a");
            assert_eq!(incoming, "model-b");
        }
        other => panic!("expected ModelMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn pinned_model_returns_none_for_empty_store() {
    let s: SemanticStore = SemanticStore::open_in_memory(None).unwrap();
    assert!(s.pinned_model().unwrap().is_none());
}

#[tokio::test]
async fn pinned_model_returns_first_row_model() {
    let e: Arc<dyn Embedder> = Arc::new(TaggedHashEmbedder {
        inner: HashEmbedder::new(32),
        model: "model-x".to_string(),
    });
    let s = SemanticStore::open_in_memory(Some(e)).unwrap();
    s.index("notes", "k1", "alpha").await.unwrap();
    assert_eq!(s.pinned_model().unwrap().as_deref(), Some("model-x"));
}

#[tokio::test]
async fn clear_all_drops_every_row_across_namespaces() {
    let s = store_with_hash(32);
    s.index("ns_a", "k1", "x").await.unwrap();
    s.index("ns_a", "k2", "y").await.unwrap();
    s.index("ns_b", "k3", "z").await.unwrap();
    s.index("ns_c", "k4", "w").await.unwrap();
    assert_eq!(s.count(None).unwrap(), 4);
    let dropped = s.clear_all().unwrap();
    assert_eq!(dropped, 4);
    assert_eq!(s.count(None).unwrap(), 0);
    assert!(s.pinned_model().unwrap().is_none());
}

#[tokio::test]
async fn clear_all_unsticks_so_new_model_can_index() {
    // Demonstrate the migration story: after clear_all, the
    // ModelMismatch protection is lifted and a different model
    // can populate the store.
    let e1: Arc<dyn Embedder> = Arc::new(TaggedHashEmbedder {
        inner: HashEmbedder::new(32),
        model: "old-model".to_string(),
    });
    let s = SemanticStore::open_in_memory(Some(e1)).unwrap();
    s.index("notes", "k", "first").await.unwrap();
    assert_eq!(s.pinned_model().unwrap().as_deref(), Some("old-model"));

    let dropped = s.clear_all().unwrap();
    assert_eq!(dropped, 1);

    // Now swap to the new model and re-index.
    let e2: Arc<dyn Embedder> = Arc::new(TaggedHashEmbedder {
        inner: HashEmbedder::new(32),
        model: "new-model".to_string(),
    });
    let s2 = s.with_embedder(e2);
    s2.index("notes", "k", "first")
        .await
        .expect("after clear_all the new model should be free to index");
    assert_eq!(s2.pinned_model().unwrap().as_deref(), Some("new-model"));
}

#[tokio::test]
async fn clear_all_on_empty_store_returns_zero() {
    let s = store_with_hash(16);
    assert_eq!(s.clear_all().unwrap(), 0);
}

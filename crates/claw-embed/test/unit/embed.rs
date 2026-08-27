use super::*;

#[tokio::test]
async fn stub_is_deterministic_and_unit_norm() {
    let embedder = StubEmbedder;
    let request = EmbedRequest {
        inputs: vec!["hello".into()],
    };
    let a = embedder.embed(request.clone()).await.unwrap();
    let b = embedder.embed(request).await.unwrap();
    assert_eq!(a.embeddings, b.embeddings);
    assert_eq!(a.model, "stub-sha256");
    assert_eq!(a.dim, EMBED_DIM);
    assert_eq!(a.embeddings[0].len(), EMBED_DIM);
    let norm = a.embeddings[0]
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "norm = {norm}");
}

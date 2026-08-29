use super::*;
use tokio::io::AsyncWriteExt as _;

#[test]
fn builds_configured_model_response() {
    let status = ModelStatus {
        ready: true,
        provider: "ollama".into(),
        model: "qwen3:8b".into(),
    };
    let catalogue = ModelCatalogue {
        providers: vec![ProviderEntry {
            name: "ollama".into(),
            label: "Local Ollama".into(),
            models: Vec::new(),
        }],
    };
    let response = build_response(&status, &catalogue);
    assert!(response.ready);
    assert_eq!(response.label, "Local Ollama · qwen3:8b");
    assert_eq!(response.models[0].id, "qwen3:8b");
}

#[tokio::test]
async fn bounded_reader_never_allocates_past_limit() {
    let (mut writer, reader) = tokio::io::duplex(64);
    let write = tokio::spawn(async move {
        let payload: &[u8] = &[b'x'; 128];
        writer.write_all(payload).await.unwrap();
    });
    let captured = read_bounded(reader, 32).await.unwrap();
    write.await.unwrap();
    assert_eq!(captured.bytes.len(), 32);
    assert!(captured.truncated);
}

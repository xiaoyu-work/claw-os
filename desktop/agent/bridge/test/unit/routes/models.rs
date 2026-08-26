use super::*;
use tokio::io::AsyncWriteExt as _;

#[test]
fn builds_configured_model_response() {
    let status = json!({
        "ready": true,
        "provider": "ollama",
        "model": "qwen3:8b",
    });
    let catalogue = json!({
        "providers": [{
            "name": "ollama",
            "label": "Local Ollama",
            "models": [],
        }],
    });
    let response = build_response(&status, &catalogue).unwrap();
    assert!(response.ready);
    assert_eq!(response.label, "Local Ollama · qwen3:8b");
    assert_eq!(response.models[0].id, "qwen3:8b");
}

#[tokio::test]
async fn bounded_reader_never_allocates_past_limit() {
    let (mut writer, reader) = tokio::io::duplex(64);
    let write = tokio::spawn(async move {
        writer.write_all(&vec![b'x'; 128]).await.unwrap();
    });
    let captured = read_bounded(reader, 32).await.unwrap();
    write.await.unwrap();
    assert_eq!(captured.bytes.len(), 32);
    assert!(captured.truncated);
}

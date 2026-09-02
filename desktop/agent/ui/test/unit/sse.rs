use super::*;
use futures::stream;

#[tokio::test]
async fn parses_delta_then_done() {
    let body = b"event: delta\ndata: {\"text\":\"Hello\"}\n\nevent: delta\ndata: {\"text\":\" world\"}\n\nevent: done\ndata: {\"answer\":\"Hello world\"}\n\n";
    let s = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(&body[..]))]);
    let decoded = sse_decode(s);
    let collected: Vec<_> = decoded
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(collected.len(), 3);
    assert!(matches!(&collected[0], StreamEvent::Delta(payload) if payload.text == "Hello"));
    assert!(matches!(&collected[1], StreamEvent::Delta(payload) if payload.text == " world"));
    assert!(matches!(&collected[2], StreamEvent::Done(_)));
}

#[tokio::test]
async fn ignores_unknown_event_names() {
    let body = b"event: ping\ndata: {}\n\nevent: delta\ndata: {\"text\":\"hi\"}\n\n";
    let s = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(&body[..]))]);
    let decoded = sse_decode(s);
    let collected: Vec<_> = decoded
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(collected.len(), 1);
}

#[tokio::test]
async fn handles_multibyte_char_split_across_chunks() {
    // "😀" encodes as F0 9F 98 80. Split the stream inside those bytes so
    // the first chunk ends mid-character — the old per-chunk from_utf8
    // would abort the whole reply here.
    let full = "event: delta\ndata: {\"text\":\"😀\"}\n\n"
        .as_bytes()
        .to_vec();
    let mid = full.len() - 6; // between the emoji's 2nd and 3rd byte
    let a = Bytes::copy_from_slice(&full[..mid]);
    let b = Bytes::copy_from_slice(&full[mid..]);
    let s = stream::iter(vec![Ok::<_, reqwest::Error>(a), Ok::<_, reqwest::Error>(b)]);
    let decoded = sse_decode(s);
    let collected: Vec<_> = decoded
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(collected.len(), 1);
    assert!(matches!(&collected[0], StreamEvent::Delta(payload) if payload.text == "😀"));
}

#[tokio::test]
async fn parses_crlf_and_final_event_without_separator() {
    let body = b"event: task\r\ndata: {\"task_id\":\"job-1\",\"session_id\":\"s-1\"}\r\n\r\nevent: warning\ndata: {\"message\":\"careful\"}";
    let s = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(&body[..]))]);
    let collected = sse_decode(s)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(matches!(
        &collected[0],
        StreamEvent::TaskStarted(payload) if payload.task_id == "job-1"
    ));
    assert!(matches!(
        &collected[1],
        StreamEvent::Warning(payload) if payload.message == "careful"
    ));
}

#[tokio::test]
async fn malformed_known_event_is_an_error() {
    let body = b"event: delta\ndata: not-json\n\n";
    let s = stream::iter(vec![Ok::<_, reqwest::Error>(Bytes::from(&body[..]))]);
    let collected = sse_decode(s).collect::<Vec<_>>().await;
    assert!(collected[0].is_err());
}

#[tokio::test]
async fn rejects_unbounded_event_buffers() {
    let body = Bytes::from(vec![b'x'; MAX_SSE_BUFFER + 1]);
    let s = stream::iter(vec![Ok::<_, reqwest::Error>(body)]);
    let collected = sse_decode(s).collect::<Vec<_>>().await;
    assert!(collected[0].is_err());
}

#[test]
fn task_event_requires_an_id() {
    let error = parse_block("event: task\ndata: {}").unwrap_err();
    assert!(error.to_string().contains("task_id"));
}

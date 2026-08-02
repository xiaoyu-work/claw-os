//! Minimal SSE parser for the `cos-agent-bridge` chat stream.
//!
//! The bridge emits `text/event-stream` with three named events:
//!
//!   * `delta` — `{type:"delta", text:"<chunk>"}` (incremental tokens)
//!   * `error` — `{type:"error", message:"…"}`
//!   * `done`  — full agent envelope (answer, turns, usage, …)
//!
//! Events are separated by `\n\n`. Within a block, lines are
//! `event: <name>` and `data: <value>` (one or more — concatenated
//! with `\n` per the SSE spec, though the bridge only emits one).
//!
//! We deliberately do not pull a full SSE crate — the format is
//! a dozen lines of byte-pushing and bringing in `eventsource-stream`
//! would drag tokio-tungstenite-style transitive deps for no reason.

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::Stream;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;

use crate::bridge::{
    BridgeEndpoint, ChatRequest, DeltaPayload, ErrorPayload, StreamEvent, bridge_url,
};

/// Open an SSE stream against the bridge and return a `Stream` of
/// decoded `StreamEvent`s. Drops on completion or transport error.
pub async fn open_chat_stream(
    endpoint: BridgeEndpoint,
    request: ChatRequest,
) -> Result<impl Stream<Item = Result<StreamEvent>>> {
    let url = bridge_url(&endpoint, "/api/chat");
    let client = Client::builder()
        .build()
        .context("building reqwest client")?;
    let response = client
        .post(&url)
        .bearer_auth(&endpoint.token)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("bridge {url} responded {status}: {body}");
    }

    let byte_stream = response.bytes_stream();
    Ok(sse_decode(byte_stream))
}

fn sse_decode<S>(byte_stream: S) -> impl Stream<Item = Result<StreamEvent>>
where
    S: Stream<Item = reqwest::Result<Bytes>>,
{
    async_stream::stream! {
        let mut byte_stream = std::pin::pin!(byte_stream);
        // Accumulate raw bytes: `reqwest`'s `bytes_stream()` splits at
        // arbitrary network boundaries, so a chunk can end in the middle of
        // a multi-byte UTF-8 character. We never decode a partial chunk —
        // only whole `\n\n`-delimited event blocks, which (the separator
        // being ASCII) always end on a code-point boundary.
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(b) => b,
                Err(e) => {
                    yield Err(anyhow::Error::from(e));
                    return;
                }
            };
            buffer.extend_from_slice(&chunk);

            loop {
                let Some(sep) = find_subslice(&buffer, b"\n\n") else { break };
                let block: Vec<u8> = buffer.drain(..sep + 2).collect();
                // `block` is one complete event (lines + trailing `\n\n`);
                // lossily decode so genuinely corrupt bytes become U+FFFD
                // instead of aborting the whole reply.
                let text = String::from_utf8_lossy(&block[..sep]);
                if let Some(event) = parse_block(text.as_ref()) {
                    yield Ok(event);
                }
            }
        }
    }
}

/// Index of the first occurrence of `needle` in `haystack`, or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_block(block: &str) -> Option<StreamEvent> {
    let mut event_name = String::from("message");
    let mut data_lines: Vec<&str> = Vec::new();
    for raw in block.split('\n') {
        let line = raw.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    let data = data_lines.join("\n");
    match event_name.as_str() {
        "delta" => serde_json::from_str::<DeltaPayload>(&data)
            .ok()
            .map(|p| StreamEvent::Delta(p.text)),
        "error" => serde_json::from_str::<ErrorPayload>(&data)
            .ok()
            .map(|p| StreamEvent::Error(p.message)),
        "done" => serde_json::from_str::<Value>(&data)
            .ok()
            .map(StreamEvent::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
        matches!(&collected[0], StreamEvent::Delta(t) if t == "Hello");
        matches!(&collected[1], StreamEvent::Delta(t) if t == " world");
        matches!(&collected[2], StreamEvent::Done(_));
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
        let full = "event: delta\ndata: {\"text\":\"😀\"}\n\n".as_bytes().to_vec();
        let mid = full.len() - 6; // between the emoji's 2nd and 3rd byte
        let a = Bytes::copy_from_slice(&full[..mid]);
        let b = Bytes::copy_from_slice(&full[mid..]);
        let s = stream::iter(vec![
            Ok::<_, reqwest::Error>(a),
            Ok::<_, reqwest::Error>(b),
        ]);
        let decoded = sse_decode(s);
        let collected: Vec<_> = decoded
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(collected.len(), 1);
        assert!(matches!(&collected[0], StreamEvent::Delta(t) if t == "😀"));
    }
}

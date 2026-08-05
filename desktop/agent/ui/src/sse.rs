//! Minimal SSE parser for the `cos-agent-bridge` chat stream.
//!
//! The bridge emits task identity, text deltas, tool lifecycle,
//! warnings, per-turn usage, errors, and a final done envelope.
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

use crate::bridge::{BridgeEndpoint, ChatRequest, DeltaPayload, StreamEvent, bridge_url};

const MAX_SSE_BUFFER: usize = 1024 * 1024;

/// Open an SSE stream against the bridge and return a `Stream` of
/// decoded `StreamEvent`s. Drops on completion or transport error.
pub async fn open_chat_stream(
    endpoint: BridgeEndpoint,
    request: ChatRequest,
) -> Result<impl Stream<Item = Result<StreamEvent>>> {
    let url = bridge_url(&endpoint, "/api/chat");
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
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
            if buffer.len() > MAX_SSE_BUFFER {
                yield Err(anyhow::anyhow!("SSE event exceeded 1 MiB"));
                return;
            }

            loop {
                let Some((sep, separator_len)) = find_event_separator(&buffer) else { break };
                let block: Vec<u8> = buffer.drain(..sep + separator_len).collect();
                // `block` is one complete event (lines + trailing `\n\n`);
                // lossily decode so genuinely corrupt bytes become U+FFFD
                // instead of aborting the whole reply.
                let text = String::from_utf8_lossy(&block[..sep]);
                match parse_block(text.as_ref()) {
                    Ok(Some(event)) => yield Ok(event),
                    Ok(None) => {}
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
        }
        if !buffer.is_empty() {
            let text = String::from_utf8_lossy(&buffer);
            match parse_block(text.as_ref()) {
                Ok(Some(event)) => yield Ok(event),
                Ok(None) => {}
                Err(error) => yield Err(error),
            }
        }
    }
}

fn find_event_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find_subslice(buffer, b"\n\n").map(|index| (index, 2));
    let crlf = find_subslice(buffer, b"\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

/// Index of the first occurrence of `needle` in `haystack`, or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_block(block: &str) -> Result<Option<StreamEvent>> {
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
        return Ok(None);
    }
    let data = data_lines.join("\n");
    let value = || {
        serde_json::from_str::<Value>(&data)
            .with_context(|| format!("decoding SSE `{event_name}` payload"))
    };
    let event = match event_name.as_str() {
        "task" => {
            let payload = value()?;
            let task_id = payload
                .get("task_id")
                .and_then(Value::as_str)
                .filter(|task_id| !task_id.is_empty())
                .context("SSE task event omitted task_id")?
                .to_string();
            StreamEvent::TaskStarted {
                task_id,
                session_id: payload
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }
        }
        "delta" => StreamEvent::Delta(
            serde_json::from_str::<DeltaPayload>(&data)
                .with_context(|| "decoding SSE `delta` payload")?
                .text,
        ),
        "text" => {
            let payload = value()?;
            StreamEvent::Delta(
                payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        "tool_use_start" => {
            let payload = value()?;
            StreamEvent::ToolUseStart {
                id: string_field(&payload, "id"),
                name: string_field(&payload, "name"),
            }
        }
        "tool_input_delta" => {
            let payload = value()?;
            StreamEvent::ToolInputDelta {
                id: string_field(&payload, "id"),
                delta: payload
                    .get("delta")
                    .or_else(|| payload.get("partial"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }
        }
        "tool_use" => {
            let payload = value()?;
            StreamEvent::ToolUse(crate::bridge::ToolCallView {
                id: string_field(&payload, "id"),
                name: string_field(&payload, "name"),
                input: payload.get("input").cloned().unwrap_or(Value::Null),
                partial_json: String::new(),
                in_progress: false,
            })
        }
        "tool_start" => {
            let payload = value()?;
            StreamEvent::ToolStart {
                id: string_field(&payload, "id"),
                name: string_field(&payload, "name"),
                input: payload.get("input").cloned().unwrap_or(Value::Null),
            }
        }
        "tool_result" => {
            let payload = value()?;
            let text = ["preview", "output", "content", "text"]
                .into_iter()
                .find_map(|field| payload.get(field).and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            let is_error = payload
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| !payload.get("ok").and_then(Value::as_bool).unwrap_or(true));
            StreamEvent::ToolResult(crate::bridge::ToolResultView {
                id: string_field(&payload, "id"),
                name: string_field(&payload, "name"),
                text,
                is_error,
            })
        }
        "warning" => {
            let payload = value()?;
            StreamEvent::Warning(
                payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        "turn_done" => StreamEvent::TurnDone(value()?),
        "error" => {
            let payload = value()?;
            StreamEvent::Error(
                payload
                    .get("message")
                    .or_else(|| payload.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("stream error")
                    .to_string(),
            )
        }
        "done" => StreamEvent::Done(value()?),
        _ => return Ok(None),
    };
    Ok(Some(event))
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
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
        assert!(matches!(&collected[0], StreamEvent::Delta(t) if t == "Hello"));
        assert!(matches!(&collected[1], StreamEvent::Delta(t) if t == " world"));
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
        assert!(matches!(&collected[0], StreamEvent::Delta(t) if t == "😀"));
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
            StreamEvent::TaskStarted { task_id, .. } if task_id == "job-1"
        ));
        assert!(matches!(
            &collected[1],
            StreamEvent::Warning(message) if message == "careful"
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
}

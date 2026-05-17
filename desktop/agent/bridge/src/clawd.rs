use std::path::Path;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Serialize)]
struct Request<'a> {
    id: u64,
    command: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<ErrorEnvelope>,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    message: String,
}

pub async fn request(socket: &Path, command: &str, params: Value) -> anyhow::Result<Value> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to clawd socket {}", socket.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let request = Request {
        id: 1,
        command,
        params,
    };
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .await
        .with_context(|| format!("writing clawd request {command}"))?;
    writer
        .flush()
        .await
        .with_context(|| format!("flushing clawd request {command}"))?;

    let Some(line) = lines
        .next_line()
        .await
        .with_context(|| format!("reading clawd response for {command}"))?
    else {
        bail!("clawd closed connection before responding to {command}");
    };
    let response: Response = serde_json::from_str(&line)
        .with_context(|| format!("decoding clawd response for {command}"))?;
    if response.ok {
        Ok(response.result)
    } else if let Some(error) = response.error {
        bail!("clawd {command}: {}", error.message)
    } else {
        bail!("clawd {command}: unknown error")
    }
}

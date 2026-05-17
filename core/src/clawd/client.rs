use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::protocol::{encode_request, Request, Response};

pub async fn request(socket_path: impl AsRef<Path>, request: Request) -> Result<Response, String> {
    let stream = UnixStream::connect(socket_path.as_ref())
        .await
        .map_err(|err| format!("failed to connect to clawd socket: {err}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let line = encode_request(&request).map_err(|err| err.to_string())?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|err| format!("failed to write clawd request: {err}"))?;
    writer
        .flush()
        .await
        .map_err(|err| format!("failed to flush clawd request: {err}"))?;

    let mut response_line = String::new();
    let read = reader
        .read_line(&mut response_line)
        .await
        .map_err(|err| format!("failed to read clawd response: {err}"))?;
    if read == 0 {
        return Err("clawd closed the connection without a response".to_string());
    }

    serde_json::from_str(response_line.trim_end()).map_err(|err| err.to_string())
}

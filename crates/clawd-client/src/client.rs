use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

#[cfg(unix)]
use std::future::Future;

use crate::protocol::{
    Request, Response, HEADER_BYTES, KIND_REQUEST, KIND_RESPONSE, MAGIC, MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES,
};
use crate::{discover_socket, ClientError, Command, Error};

#[derive(Debug, Clone, Copy)]
pub struct ClientConfig {
    pub connect_timeout: Duration,
    pub write_timeout: Duration,
    pub read_timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(30),
            max_request_bytes: MAX_REQUEST_BYTES,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    config: ClientConfig,
}

impl Client {
    pub fn from_env() -> Result<Self, ClientError> {
        discover_socket().map(Self::new)
    }

    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self::with_config(socket, ClientConfig::default())
    }

    pub fn with_config(socket: impl Into<PathBuf>, config: ClientConfig) -> Self {
        Self {
            socket: socket.into(),
            config,
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub async fn call(&self, command: Command, params: Value) -> Result<Value, Error> {
        self.exchange(Request::new(command, params))
            .await?
            .into_result()
    }

    #[cfg(unix)]
    pub async fn exchange(&self, request: Request) -> Result<Response, ClientError> {
        use tokio::net::UnixStream;

        let path = self.socket.display().to_string();
        let stream = connect_with_timeout(
            path,
            self.config.connect_timeout,
            UnixStream::connect(&self.socket),
        )
        .await?;
        exchange_on_stream(stream, request, self.config).await
    }

    #[cfg(not(unix))]
    pub async fn exchange(&self, _request: Request) -> Result<Response, ClientError> {
        Err(ClientError::UnsupportedPlatform)
    }
}

#[cfg(unix)]
async fn connect_with_timeout<T, F>(
    path: String,
    duration: Duration,
    connect: F,
) -> Result<T, ClientError>
where
    F: Future<Output = std::io::Result<T>>,
{
    tokio::time::timeout(duration, connect)
        .await
        .map_err(|_| ClientError::ConnectTimeout { path: path.clone() })?
        .map_err(|source| ClientError::Connect { path, source })
}

#[cfg(unix)]
async fn exchange_on_stream(
    mut stream: tokio::net::UnixStream,
    request: Request,
    config: ClientConfig,
) -> Result<Response, ClientError> {
    use tokio::io::AsyncWriteExt;

    let body = serde_json::to_vec(&request).map_err(ClientError::Encode)?;
    let maximum = config.max_request_bytes.min(u32::MAX as usize);
    if body.len() > maximum {
        return Err(ClientError::RequestTooLarge {
            actual: body.len(),
            maximum,
        });
    }
    let frame = encode_frame(KIND_REQUEST, &body);
    tokio::time::timeout(config.write_timeout, async {
        stream.write_all(&frame).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| ClientError::WriteTimeout)?
    .map_err(ClientError::Write)?;

    let body = tokio::time::timeout(
        config.read_timeout,
        read_response(&mut stream, config.max_response_bytes),
    )
    .await
    .map_err(|_| ClientError::ReadTimeout)??;
    let response =
        serde_json::from_slice::<Response>(&body).map_err(ClientError::MalformedResponse)?;
    response.validate(&request.id)
}

fn encode_frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_BYTES + body.len());
    frame.extend_from_slice(&MAGIC);
    frame.push(kind);
    frame.push(0);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

#[cfg(unix)]
async fn read_response(
    stream: &mut tokio::net::UnixStream,
    max: usize,
) -> Result<Vec<u8>, ClientError> {
    let mut header = [0u8; HEADER_BYTES];
    read_exact(stream, &mut header).await?;
    if header[..4] != MAGIC || header[4] != KIND_RESPONSE || header[5] != 0 {
        return Err(ClientError::UnsupportedFrame);
    }
    let len = u32::from_be_bytes([header[6], header[7], header[8], header[9]]) as usize;
    if len > max {
        return Err(ClientError::ResponseTooLarge {
            actual: len,
            maximum: max,
        });
    }
    let mut body = vec![0u8; len];
    read_exact(stream, &mut body).await?;
    Ok(body)
}

#[cfg(unix)]
async fn read_exact(
    stream: &mut tokio::net::UnixStream,
    buffer: &mut [u8],
) -> Result<(), ClientError> {
    use tokio::io::AsyncReadExt;

    stream
        .read_exact(buffer)
        .await
        .map(|_| ())
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ) {
                ClientError::TruncatedResponse
            } else {
                ClientError::Read(error)
            }
        })
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/client.rs"));
}

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use super::protocol::{
    decode, error_response, notification, response, Incoming, RequestId, RpcError,
    MAX_MESSAGE_BYTES,
};
use super::server::{Connection, Outcome, Service};

const MAX_CONNECTIONS: usize = 4;
const MAX_REQUESTS: usize = 16;
const MAX_CONTROLS: usize = 8;
const EVENT_QUEUE: usize = 64;
const HANDSHAKE_BYTES: usize = 16 * 1024;

pub(super) async fn serve(
    listener: UnixListener,
    service: Arc<Service>,
    uid: u32,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut clients = JoinSet::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
            result = clients.join_next(), if !clients.is_empty() => {
                if let Some(Err(_)) = result {
                    tracing::warn!("Claw TUI connection task failed");
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|error| format!("Accept Claw TUI client: {error}"))?;
                let credentials = match stream.peer_cred() {
                    Ok(credentials) => credentials,
                    Err(_) => continue,
                };
                if !peer_allowed(uid, credentials.uid())
                    || !credentials.pid().is_some_and(process_peer_allowed)
                {
                    continue;
                }
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                let connection = Arc::new(Connection::new(service.clone()));
                let shutdown = shutdown.clone();
                clients.spawn(async move {
                    let _slot = slot;
                    if run_connection(stream, connection, shutdown).await.is_err() {
                        tracing::debug!("Claw TUI client connection closed");
                    }
                });
            }
        }
    }
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    Ok(())
}

fn peer_allowed(server_uid: u32, peer_uid: u32) -> bool {
    server_uid != 0 && peer_uid == server_uid
}

fn process_peer_allowed(pid: i32) -> bool {
    use std::io::Read;

    let Ok(pid) = u32::try_from(pid) else {
        return false;
    };
    let owner = std::process::id();
    if pid == owner {
        return true;
    }
    // The launcher owns the upstream child. A same-uid agentd worker is not an
    // interactive owner principal and must not use this adapter as a task proxy.
    let Ok(file) = std::fs::File::open(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let mut stat = String::new();
    if file.take(4097).read_to_string(&mut stat).is_err() || stat.len() > 4096 {
        return false;
    }
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(1))
        .and_then(|ppid| ppid.parse::<u32>().ok())
        == Some(owner)
}

enum Finished {
    Request {
        id: RequestId,
        result: Result<Outcome, RpcError>,
    },
    Answer(Result<Vec<Value>, RpcError>),
}

pub(super) async fn run_connection(
    stream: UnixStream,
    connection: Arc<Connection>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_BYTES);
    config.max_frame_size = Some(MAX_MESSAGE_BYTES);
    config.write_buffer_size = 4096;
    config.max_write_buffer_size = 2 * MAX_MESSAGE_BYTES;
    let guarded = HandshakeStream {
        inner: stream,
        remaining: Some(HANDSHAKE_BYTES),
    };
    let mut websocket = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::accept_async_with_config(guarded, Some(config)),
    )
    .await
    .map_err(|_| "TUI WebSocket handshake timed out".to_string())?
    .map_err(|_| "Invalid TUI WebSocket handshake".to_string())?;
    websocket.get_mut().remaining = None;
    let (mut sink, mut incoming) = websocket.split();
    let (outgoing, mut messages) = mpsc::channel::<Value>(EVENT_QUEUE);
    let (control, mut controls) = mpsc::channel::<Message>(8);
    let mut writer = tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                value = messages.recv() => {
                    let Some(value) = value else { break };
                    let encoded = serde_json::to_string(&value)
                        .map_err(|_| "Could not encode TUI message".to_string())?;
                    if encoded.len() > MAX_MESSAGE_BYTES {
                        return Err("TUI message exceeds its size limit".to_string());
                    }
                    Message::Text(encoded.into())
                }
                control = controls.recv() => {
                    let Some(control) = control else { break };
                    control
                }
            };
            let closing = matches!(frame, Message::Close(_));
            tokio::time::timeout(Duration::from_secs(10), sink.send(frame))
                .await
                .map_err(|_| "TUI stopped reading messages".to_string())?
                .map_err(|_| "TUI WebSocket write failed".to_string())?;
            if closing {
                break;
            }
        }
        Ok::<(), String>(())
    });
    let mut requests = JoinSet::<Finished>::new();
    let mut control_requests = JoinSet::<Finished>::new();
    let mut drives = JoinSet::new();
    let mut pending = HashSet::new();
    let result = async {
        loop {
            if *shutdown.borrow() { return Ok(()); }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                result = &mut writer => {
                    return result.map_err(|_| "TUI writer failed".to_string())?;
                }
                completed = requests.join_next(), if !requests.is_empty() => {
                    finish(completed, &connection, &outgoing, &mut pending, &mut drives).await?;
                }
                completed = control_requests.join_next(), if !control_requests.is_empty() => {
                    finish(completed, &connection, &outgoing, &mut pending, &mut drives).await?;
                }
                completed = drives.join_next(), if !drives.is_empty() => {
                    if completed.is_some_and(|result| result.is_err()) {
                        return Err("TUI task subscription failed".to_string());
                    }
                }
                frame = incoming.next() => {
                    let Some(frame) = frame else { return Ok(()); };
                    match frame.map_err(|_| "Invalid or oversized TUI WebSocket frame".to_string())? {
                        Message::Text(text) => match decode(&text) {
                            Err((id, error)) => send(&outgoing, error_response(id.as_ref(), error)).await?,
                            Ok(Incoming::Notification { method, params }) => {
                                if let Err(error) = connection.notify(&method, params) {
                                    connection.emit(&outgoing, notification("warning", json!({
                                        "threadId": null, "message": error.message,
                                    }))).await.map_err(|error| error.message)?;
                                }
                            }
                            Ok(Incoming::Response { id, result }) => {
                                if control_requests.len() >= MAX_CONTROLS {
                                    return Err("Too many pending approval responses".to_string());
                                }
                                let connection = connection.clone();
                                control_requests.spawn(async move {
                                    Finished::Answer(connection.answer(id, result).await)
                                });
                            }
                            Ok(Incoming::Request { id, method, params }) => {
                                if pending.contains(&id) {
                                    send(&outgoing, error_response(Some(&id), RpcError::invalid_request("Duplicate in-flight request id"))).await?;
                                    continue;
                                }
                                let controls = method == "turn/interrupt";
                                if (controls && control_requests.len() >= MAX_CONTROLS)
                                    || (!controls && requests.len() >= MAX_REQUESTS)
                                {
                                    send(&outgoing, error_response(Some(&id), RpcError::capacity())).await?;
                                    continue;
                                }
                                pending.insert(id.clone());
                                let connection = connection.clone();
                                let work = async move {
                                    let result = connection.execute(&method, params).await;
                                    Finished::Request { id, result }
                                };
                                if controls { control_requests.spawn(work); } else { requests.spawn(work); }
                            }
                        },
                        Message::Ping(payload) => {
                            control.try_send(Message::Pong(payload))
                                .map_err(|_| "Too many TUI control frames".to_string())?;
                        }
                        Message::Pong(_) => {}
                        Message::Close(_) => return Ok(()),
                        Message::Binary(_) | Message::Frame(_) => {
                            return Err("Only text JSON-RPC WebSocket messages are supported".to_string());
                        }
                    }
                }
            }
        }
    }.await;
    requests.abort_all();
    control_requests.abort_all();
    drives.abort_all();
    while requests.join_next().await.is_some() {}
    while control_requests.join_next().await.is_some() {}
    while drives.join_next().await.is_some() {}
    writer.abort();
    result
}

async fn finish(
    completed: Option<Result<Finished, tokio::task::JoinError>>,
    connection: &Arc<Connection>,
    outgoing: &mpsc::Sender<Value>,
    pending: &mut HashSet<RequestId>,
    drives: &mut JoinSet<()>,
) -> Result<(), String> {
    let completed = completed
        .ok_or_else(|| "TUI request task disappeared".to_string())?
        .map_err(|_| "TUI request task failed".to_string())?;
    match completed {
        Finished::Answer(result) => match result {
            Ok(messages) => {
                for message in messages {
                    connection
                        .emit(outgoing, message)
                        .await
                        .map_err(|error| error.message)?;
                }
            }
            Err(error) => connection
                .emit(
                    outgoing,
                    notification(
                        "warning",
                        json!({
                            "threadId": null, "message": error.message,
                        }),
                    ),
                )
                .await
                .map_err(|error| error.message)?,
        },
        Finished::Request { id, result } => {
            pending.remove(&id);
            match result {
                Err(error) => send(outgoing, error_response(Some(&id), error)).await?,
                Ok(outcome) => {
                    let value = response(&id, outcome.result);
                    if value.to_string().len() > MAX_MESSAGE_BYTES {
                        send(outgoing, error_response(Some(&id), RpcError::capacity())).await?;
                        return Ok(());
                    }
                    send(outgoing, value).await?;
                    for notification in outcome.notifications {
                        connection
                            .emit(outgoing, notification)
                            .await
                            .map_err(|error| error.message)?;
                    }
                    if let Some(drive) = outcome.drive {
                        let connection = connection.clone();
                        let outgoing = outgoing.clone();
                        drives.spawn(async move {
                            connection.drive(drive, outgoing).await;
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

async fn send(outgoing: &mpsc::Sender<Value>, value: Value) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), outgoing.send(value))
        .await
        .map_err(|_| "TUI response queue is full".to_string())?
        .map_err(|_| "TUI writer disconnected".to_string())
}

/// Tungstenite bounds frames after upgrade; separately bound the HTTP upgrade
/// bytes so an authenticated local peer cannot grow an unfinished header.
struct HandshakeStream {
    inner: UnixStream,
    remaining: Option<usize>,
}

impl AsyncRead for HandshakeStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(remaining) = self.remaining else {
            return Pin::new(&mut self.inner).poll_read(cx, buffer);
        };
        if remaining == 0 {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WebSocket upgrade is too large",
            )));
        }
        let mut scratch = [0; 4096];
        let length = remaining.min(scratch.len()).min(buffer.remaining());
        let mut read = ReadBuf::new(&mut scratch[..length]);
        match Pin::new(&mut self.inner).poll_read(cx, &mut read) {
            Poll::Ready(Ok(())) => {
                let count = read.filled().len();
                buffer.put_slice(read.filled());
                self.remaining = Some(remaining - count);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncWrite for HandshakeStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/transport.rs"
    ));
}

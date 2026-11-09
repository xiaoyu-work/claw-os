//! OS-owned multiplexing between Thunderbird and the AI-only App host.
//! Only root broker requests can become mailbox commands; App stdout cannot.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

pub(crate) const SOCKET_NAME: &str = "claw-thunderbird.sock";
pub(crate) const MAX_MAIL_FRAME: usize = 32 * 1024 * 1024;
const MAX_AI_FRAME: usize = 8 * 1024 * 1024;
const MAX_HOST_FRAME: usize = 1024 * 1024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const MAIL_TIMEOUT: Duration = Duration::from_secs(35);

struct SocketGuard {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketGuard {
    fn open() -> Result<Self, String> {
        let uid = unsafe { libc::getuid() };
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        let metadata = fs::symlink_metadata(&runtime).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err("Thunderbird runtime must be an owner-only directory".into());
        }
        let path = runtime.join(SOCKET_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket()
                    || metadata.uid() != uid
                    || metadata.mode() & 0o077 != 0
                {
                    return Err("Thunderbird bridge path is not a private owner socket".into());
                }
                match UnixStream::connect(&path) {
                    Ok(_) => return Err("a Thunderbird mailbox bridge is already running".into()),
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(&path).map_err(|e| e.to_string())?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        Ok(Self {
            listener,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn poll(fds: &mut [libc::pollfd], milliseconds: i32) -> Result<(), String> {
    loop {
        let result =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, milliseconds) };
        if result >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
    }
}

fn read_exact<R: Read + AsRawFd>(input: &mut R, bytes: &mut [u8]) -> Result<bool, String> {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    let mut position = 0;
    while position < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("native frame read deadline elapsed".into());
        }
        let mut fd = [libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        poll(&mut fd, remaining.as_millis().min(i32::MAX as u128) as i32)?;
        if fd[0].revents == 0 {
            continue;
        }
        match input.read(&mut bytes[position..]) {
            Ok(0) if position == 0 => return Ok(false),
            Ok(0) => return Err("truncated native frame".into()),
            Ok(count) => position += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(true)
}

fn read_frame<R: Read + AsRawFd>(input: &mut R, limit: usize) -> Result<Option<Value>, String> {
    let mut header = [0; 4];
    if !read_exact(input, &mut header)? {
        return Ok(None);
    }
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > limit {
        return Err("native frame exceeds its declared limit".into());
    }
    let mut body = vec![0; length];
    if !read_exact(input, &mut body)? {
        return Err("truncated native frame body".into());
    }
    let value: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("native frame must contain an object".into());
    }
    Ok(Some(value))
}

fn write_frame<W: Write>(output: &mut W, value: &Value, limit: usize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("native output exceeds its declared limit".into());
    }
    output
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    output.write_all(&bytes).map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())
}

fn request_id(value: &Value) -> Result<&str, String> {
    value["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or_else(|| "native request id is invalid".into())
}

fn validate_request(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("native request must be an object")?;
    request_id(value)?;
    if object.len() != 3
        || !object.contains_key("verb")
        || !object.contains_key("args")
        || !value["verb"]
            .as_str()
            .is_some_and(|verb| !verb.is_empty() && verb.len() <= 128)
        || !value["args"].is_object()
    {
        return Err("native request must contain exactly id, verb and object args".into());
    }
    Ok(())
}

fn validate_ai_response(value: &Value, pending: &mut HashSet<String>) -> Result<(), String> {
    let id = request_id(value)?;
    let object = value.as_object().ok_or("AI response must be an object")?;
    if object
        .keys()
        .any(|key| !["id", "ok", "result", "error", "detail"].contains(&key.as_str()))
        || !value["ok"].is_boolean()
        || !pending.remove(id)
    {
        return Err("AI host emitted an unsolicited or non-AI response".into());
    }
    Ok(())
}

fn root_peer(stream: &UnixStream) -> Result<bool, String> {
    let mut credential: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credential).cast(),
            &mut size,
        )
    };
    if result != 0 || size as usize != std::mem::size_of::<libc::ucred>() {
        return Err("could not authenticate native mailbox peer".into());
    }
    Ok(credential.uid == 0)
}

struct PendingMail {
    id: String,
    stream: UnixStream,
    deadline: Instant,
}

fn reject(stream: &mut UnixStream, id: &str, error: &str, dispatched: bool) -> Result<(), String> {
    write_frame(
        stream,
        &json!({
            "id": id, "ok": false, "error": error, "dispatched": dispatched,
        }),
        MAX_MAIL_FRAME,
    )
}

pub fn run(child: &mut Child) -> Result<(), String> {
    let mut ai_input = child.stdin.take().ok_or("AI host stdin is unavailable")?;
    let mut ai_output = child.stdout.take().ok_or("AI host stdout is unavailable")?;
    let stdout = std::io::stdout();
    // StdinLock may prefetch a frame body before poll sees it. Use an
    // unbuffered duplicate so readiness and reads observe the same stream.
    let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut input = unsafe { fs::File::from_raw_fd(fd) };
    let mut output = stdout.lock();
    serve(&mut input, &mut output, &mut ai_input, &mut ai_output)
}

fn serve<N: Read + AsRawFd, O: Write, A: Write, B: Read + AsRawFd>(
    input: &mut N,
    output: &mut O,
    ai_input: &mut A,
    ai_output: &mut B,
) -> Result<(), String> {
    let socket = SocketGuard::open()?;
    serve_on(&socket.listener, input, output, ai_input, ai_output)
}

fn serve_on<N: Read + AsRawFd, O: Write, A: Write, B: Read + AsRawFd>(
    listener: &UnixListener,
    input: &mut N,
    output: &mut O,
    ai_input: &mut A,
    ai_output: &mut B,
) -> Result<(), String> {
    let mut ai_pending = HashSet::new();
    let mut mail_pending: Option<PendingMail> = None;
    let mut ready = false;
    loop {
        if crate::bridge::stdio_cancelled() {
            return Err("Native Messaging transport was cancelled".into());
        }
        let mut fds = [
            libc::pollfd {
                fd: input.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: ai_output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        poll(&mut fds, 100)?;
        if fds[0].revents != 0 {
            let Some(mut message) = read_frame(input, MAX_MAIL_FRAME)? else {
                return Ok(());
            };
            match message["type"].as_str() {
                Some("mailbox.ready") => {
                    if message.as_object().is_none_or(|object| object.len() != 2)
                        || message["version"] != 1
                    {
                        return Err("invalid Thunderbird mailbox readiness".into());
                    }
                    ready = true;
                }
                Some("mailbox.response") => {
                    let id = request_id(&message)?.to_string();
                    let Some(mut pending) = mail_pending.take() else {
                        return Err("unsolicited Thunderbird mailbox response".into());
                    };
                    if pending.id != id
                        || !message["ok"].is_boolean()
                        || message.as_object().unwrap().keys().any(|key| {
                            !["type", "id", "ok", "result", "error"].contains(&key.as_str())
                        })
                    {
                        return Err("Thunderbird mailbox response did not correlate".into());
                    }
                    let object = message.as_object_mut().unwrap();
                    object.remove("type");
                    object.insert("dispatched".into(), Value::Bool(true));
                    write_frame(&mut pending.stream, &message, MAX_MAIL_FRAME)?;
                }
                Some(_) => return Err("unsupported Thunderbird native frame type".into()),
                None => {
                    validate_request(&message)?;
                    let id = request_id(&message)?.to_string();
                    if ai_pending.len() >= 32 || !ai_pending.insert(id) {
                        return Err("too many or duplicate AI requests".into());
                    }
                    write_frame(ai_input, &message, MAX_AI_FRAME)?;
                }
            }
        }
        if fds[1].revents != 0 {
            let Some(message) = read_frame(ai_output, MAX_AI_FRAME)? else {
                return Err("AI-only App host disconnected".into());
            };
            validate_ai_response(&message, &mut ai_pending)?;
            write_frame(output, &message, MAX_HOST_FRAME)?;
        }
        if fds[2].revents != 0 {
            let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
            if !root_peer(&stream)? {
                continue;
            }
            stream
                .set_write_timeout(Some(FRAME_TIMEOUT))
                .map_err(|e| e.to_string())?;
            let Some(mut message) = read_frame(&mut stream, MAX_HOST_FRAME)? else {
                continue;
            };
            validate_request(&message)?;
            let id = request_id(&message)?.to_string();
            if !ready || mail_pending.is_some() {
                reject(
                    &mut stream,
                    &id,
                    if !ready {
                        "Thunderbird requires the updated mailbox extension"
                    } else {
                        "Thunderbird mailbox bridge is busy"
                    },
                    false,
                )?;
                continue;
            }
            message
                .as_object_mut()
                .unwrap()
                .insert("type".into(), json!("mailbox.request"));
            write_frame(output, &message, MAX_HOST_FRAME)?;
            mail_pending = Some(PendingMail {
                id,
                stream,
                deadline: Instant::now() + MAIL_TIMEOUT,
            });
        }
        if mail_pending
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
        {
            let mut pending = mail_pending.take().unwrap();
            reject(
                &mut pending.stream,
                &pending.id,
                "Thunderbird mailbox outcome is unknown",
                true,
            )?;
            return Err("Thunderbird mailbox response deadline elapsed".into());
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/mail_native.rs"
    ));
}

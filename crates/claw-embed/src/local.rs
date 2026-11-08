//! Closed local embedding pipe. Concrete model loading remains in core.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{EmbedError, EmbedRequest, EmbedResponse, Embedder};

pub const LOCAL_MODEL: &str = "Qwen3-Embedding-0.6B";
pub const LOCAL_DIM: usize = 1024;
pub const MAX_INPUTS: usize = 16;
pub const MAX_INPUT_CHARS: usize = 8192;
pub const MAX_INPUT_TOKENS: usize = 2048;
pub const MAX_FILE_CHUNK_CHARS: usize = 448;
pub const MAX_PIPE_BYTES: usize = 1024 * 1024;
pub const LOCAL_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalFrame<T> {
    pub version: u32,
    pub message: T,
}

impl<T> LocalFrame<T> {
    pub fn new(message: T) -> Self {
        Self {
            version: LOCAL_PROTOCOL_VERSION,
            message,
        }
    }

    pub fn into_message(self) -> Result<T, String> {
        if self.version != LOCAL_PROTOCOL_VERSION {
            return Err(format!(
                "unsupported local embedding protocol version: {}",
                self.version
            ));
        }
        Ok(self.message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalRequest {
    Status {},
    Embed { inputs: Vec<String> },
}

impl LocalRequest {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Embed { inputs } = self {
            if inputs.is_empty()
                || inputs.len() > MAX_INPUTS
                || inputs.iter().any(|text| {
                    text.trim().is_empty()
                        || text.contains('\0')
                        || text.chars().count() > MAX_INPUT_CHARS
                })
            {
                return Err("local embedding requires 1-16 nonempty, bounded text inputs".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStatus {
    pub model: String,
    pub dim: usize,
    pub configured: bool,
    pub detail: Option<String>,
}

// External tags decode vectors directly instead of buffering arbitrary-precision
// JSON numbers as maps inside an internally tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalReply {
    Status { status: LocalStatus },
    Embedded { response: EmbedResponse },
    Error { message: String },
}

pub fn validate_embeddings(response: &EmbedResponse, count: usize) -> Result<(), String> {
    if response.model != LOCAL_MODEL
        || response.dim != LOCAL_DIM
        || response.embeddings.len() != count
        || response.embeddings.iter().any(|vector| {
            vector.len() != LOCAL_DIM
                || vector.iter().any(|value| !value.is_finite())
                || vector.iter().all(|value| *value == 0.0)
        })
    {
        return Err(
            "local embedding response has invalid model, dimensions, count or vectors".into(),
        );
    }
    Ok(())
}

struct Process {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    frame: Vec<u8>,
}

pub struct LocalEmbedder {
    process: Mutex<Option<Process>>,
    owner: Option<(u32, u32, std::path::PathBuf)>,
    configured: AtomicBool,
}

impl Default for LocalEmbedder {
    fn default() -> Self {
        Self {
            process: Mutex::new(None),
            owner: None,
            configured: AtomicBool::new(false),
        }
    }
}

impl LocalEmbedder {
    pub fn for_owner(uid: u32, gid: u32, home: std::path::PathBuf) -> Self {
        Self {
            owner: Some((uid, gid, home)),
            ..Self::default()
        }
    }

    fn spawn(&self) -> Result<Process, String> {
        let mut command = Command::new("/usr/local/bin/cos");
        command
            .arg("__embed-local")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        if let Some((uid, gid, home)) = &self.owner {
            command
                .env_clear()
                .env("HOME", home)
                .env("COS_DATA_DIR", "/var/lib/cos")
                .env("PATH", "/usr/local/bin:/usr/bin:/bin");
            let (uid, gid) = (*uid, *gid);
            unsafe {
                command.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setresgid(gid, gid, gid) != 0
                        || libc::setresuid(uid, uid, uid) != 0
                        || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::umask(0o077);
                    Ok(())
                });
            }
        } else if std::env::var_os("COS_DATA_DIR").is_none() {
            command.env("COS_DATA_DIR", "/var/lib/cos");
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("start local embedding worker: {e}"))?;
        let input = child
            .stdin
            .take()
            .ok_or("embedding worker has no input pipe")?;
        let output = child
            .stdout
            .take()
            .ok_or("embedding worker has no output pipe")?;
        Ok(Process {
            child,
            input,
            output: BufReader::new(output),
            frame: Vec::new(),
        })
    }

    async fn exchange(&self, request: LocalRequest) -> Result<LocalReply, String> {
        request.validate()?;
        let mut process = self.process.lock().await;
        if process.is_none() {
            *process = Some(self.spawn()?);
        }
        let current = process.as_mut().ok_or("embedding worker is unavailable")?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let bytes = serde_json::to_vec(&LocalFrame::new(request)).map_err(|e| e.to_string())?;
            if bytes.len() > MAX_PIPE_BYTES {
                return Err("embedding request exceeds its byte limit".into());
            }
            current
                .input
                .write_all(&bytes)
                .await
                .map_err(|e| e.to_string())?;
            current
                .input
                .write_all(b"\n")
                .await
                .map_err(|e| e.to_string())?;
            current.input.flush().await.map_err(|e| e.to_string())?;
            current.frame.clear();
            loop {
                let available = current.output.fill_buf().await.map_err(|e| e.to_string())?;
                if available.is_empty() {
                    return Err("local embedding worker closed its pipe".into());
                }
                let take = available
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|n| n + 1)
                    .unwrap_or(available.len());
                if current.frame.len() + take > MAX_PIPE_BYTES {
                    return Err("local embedding response exceeds its byte limit".into());
                }
                let done = available[take - 1] == b'\n';
                current.frame.extend_from_slice(&available[..take]);
                current.output.consume(take);
                if done {
                    break;
                }
            }
            serde_json::from_slice::<LocalFrame<LocalReply>>(&current.frame)
                .map_err(|e| format!("invalid local embedding response: {e}"))?
                .into_message()
        })
        .await
        .map_err(|_| "local embedding worker timed out".to_string())
        .and_then(|result| result);
        if result.is_err() {
            current
                .child
                .kill()
                .await
                .map_err(|e| format!("embedding failed and worker retirement failed: {e}"))?;
            current
                .child
                .wait()
                .await
                .map_err(|e| format!("embedding worker reap failed: {e}"))?;
            *process = None;
            self.configured.store(false, Ordering::SeqCst);
        }
        result
    }

    pub async fn status(&self) -> Result<LocalStatus, String> {
        match self.exchange(LocalRequest::Status {}).await? {
            LocalReply::Status { status }
                if status.model == LOCAL_MODEL && status.dim == LOCAL_DIM =>
            {
                self.configured.store(status.configured, Ordering::SeqCst);
                Ok(status)
            }
            LocalReply::Error { message } => Err(message),
            _ => Err("invalid local embedding status response".into()),
        }
    }
}

#[async_trait]
impl Embedder for LocalEmbedder {
    fn name(&self) -> &str {
        "qwen3-local"
    }
    fn model(&self) -> &str {
        LOCAL_MODEL
    }
    fn is_configured(&self) -> bool {
        self.configured.load(Ordering::SeqCst)
    }

    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
        let count = request.inputs.len();
        match self
            .exchange(LocalRequest::Embed {
                inputs: request.inputs,
            })
            .await
            .map_err(EmbedError::Transport)?
        {
            LocalReply::Embedded { response } => {
                validate_embeddings(&response, count).map_err(EmbedError::Parse)?;
                self.configured.store(true, Ordering::SeqCst);
                Ok(response)
            }
            LocalReply::Error { message } => Err(EmbedError::Transport(message)),
            _ => Err(EmbedError::Parse("unexpected local embedding reply".into())),
        }
    }
}

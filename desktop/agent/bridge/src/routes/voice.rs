//! `POST /api/voice/upload` — transcribe a recorded audio blob.

use std::{
    env,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Read as _},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use axum::{
    Json,
    body::Bytes,
    extract::{State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use cos_agent_protocol::{ErrorCode, ErrorEnvelope, VoiceResponse};
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

use crate::state::AppState;

const DEFAULT_COS_BIN: &str = "/usr/local/bin/cos";
const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);
const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
const TEMP_FILE_ATTEMPTS: usize = 16;

#[derive(Debug)]
pub(crate) enum VoiceApiError {
    InvalidBody,
    EmptyBody,
    PayloadTooLarge,
    UnsupportedMediaType,
    RuntimeUnavailable,
    BackendUnavailable,
    BackendTimeout,
    BackendFailed,
    InvalidBackendResponse,
    CleanupFailed,
}

impl IntoResponse for VoiceApiError {
    fn into_response(self) -> Response {
        let (status, code, error, hint) = match self {
            Self::InvalidBody => (
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "invalid audio body",
                None,
            ),
            Self::EmptyBody => (
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "audio body is empty",
                None,
            ),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::PayloadTooLarge,
                "audio body exceeds 25 MiB",
                Some("Keep recordings under two minutes."),
            ),
            Self::UnsupportedMediaType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ErrorCode::UnsupportedMediaType,
                "unsupported audio media type",
                Some("Record or upload WAV, MP3, M4A, FLAC, OGG, or WebM audio."),
            ),
            Self::RuntimeUnavailable => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "voice upload storage is unavailable",
                Some("Restart cos-agent-bridge.service and try again."),
            ),
            Self::BackendUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::ServiceUnavailable,
                "voice transcription service is unavailable",
                Some("Configure speech-to-text in Agent settings."),
            ),
            Self::BackendTimeout => (
                StatusCode::GATEWAY_TIMEOUT,
                ErrorCode::Timeout,
                "voice transcription timed out",
                Some("Try a shorter recording."),
            ),
            Self::BackendFailed => (
                StatusCode::BAD_GATEWAY,
                ErrorCode::UpstreamError,
                "voice transcription failed",
                Some("Configure speech-to-text in Agent settings or retry."),
            ),
            Self::InvalidBackendResponse => (
                StatusCode::BAD_GATEWAY,
                ErrorCode::UpstreamError,
                "invalid response from voice transcription service",
                Some("Check the configured speech-to-text provider."),
            ),
            Self::CleanupFailed => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "failed to clean up voice upload",
                Some("Restart cos-agent-bridge.service and try again."),
            ),
        };
        let mut envelope = ErrorEnvelope::new(code, error);
        if let Some(hint) = hint {
            envelope = envelope.with_hint(hint);
        }
        (status, Json(envelope)).into_response()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ValidationError {
    EmptyBody,
    PayloadTooLarge,
    UnsupportedMediaType,
}

#[derive(Debug, PartialEq, Eq)]
struct AudioFormat {
    mime_type: String,
    extension: &'static str,
}

#[derive(Debug)]
enum TranscriptionError {
    Spawn(io::Error),
    Communicate(io::Error),
    Timeout,
    StdoutTooLarge,
    ExitFailure {
        code: Option<i32>,
        stderr_bytes: usize,
        stderr_truncated: bool,
    },
    InvalidResponse(TranscriptParseError),
}

#[derive(Debug, PartialEq, Eq)]
enum TranscriptParseError {
    InvalidJson,
    MissingText,
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

struct TempAudio {
    path: PathBuf,
    cleaned: bool,
}

impl TempAudio {
    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(mut self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.cleaned = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.cleaned = true;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for TempAudio {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub async fn upload(
    State(_state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<VoiceResponse>, VoiceApiError> {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => {
            let status = rejection.into_response().status();
            return Err(if status == StatusCode::PAYLOAD_TOO_LARGE {
                VoiceApiError::PayloadTooLarge
            } else {
                VoiceApiError::InvalidBody
            });
        }
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let format = validate_upload(body.len(), content_type).map_err(|error| match error {
        ValidationError::EmptyBody => VoiceApiError::EmptyBody,
        ValidationError::PayloadTooLarge => VoiceApiError::PayloadTooLarge,
        ValidationError::UnsupportedMediaType => VoiceApiError::UnsupportedMediaType,
    })?;

    let runtime_dir = bridge_runtime_dir().map_err(|error| {
        tracing::error!(%error, "voice upload runtime directory is unavailable");
        VoiceApiError::RuntimeUnavailable
    })?;
    let temp = create_temp_audio(&runtime_dir, format.extension, &body)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to create private voice upload");
            VoiceApiError::RuntimeUnavailable
        })?;

    let transcription = transcribe_audio(temp.path()).await;
    if let Err(error) = temp.cleanup() {
        tracing::error!(%error, "failed to remove private voice upload");
        return Err(VoiceApiError::CleanupFailed);
    }
    let text = transcription.map_err(map_transcription_error)?;

    Ok(Json(VoiceResponse {
        text,
        bytes_received: body.len(),
        mime_type: format.mime_type,
        placeholder: false,
    }))
}

fn validate_upload(
    body_len: usize,
    content_type: Option<&str>,
) -> Result<AudioFormat, ValidationError> {
    if body_len == 0 {
        return Err(ValidationError::EmptyBody);
    }
    if body_len > super::VOICE_MAX_BYTES {
        return Err(ValidationError::PayloadTooLarge);
    }
    let content_type = content_type.ok_or(ValidationError::UnsupportedMediaType)?;
    select_audio_format(content_type).ok_or(ValidationError::UnsupportedMediaType)
}

fn select_audio_format(content_type: &str) -> Option<AudioFormat> {
    let mime_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let extension = match mime_type.as_str() {
        "audio/wav" | "audio/wave" | "audio/x-wav" | "audio/vnd.wave" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        _ => return None,
    };
    Some(AudioFormat {
        mime_type,
        extension,
    })
}

fn bridge_runtime_dir() -> io::Result<PathBuf> {
    let runtime_root = env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is required for voice uploads",
        )
    })?;
    let runtime_root = PathBuf::from(runtime_root);
    if !runtime_root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "XDG_RUNTIME_DIR must be absolute",
        ));
    }

    let directory = runtime_root.join("cos-agent-bridge");
    validate_private_directory(&directory)?;
    Ok(directory)
}

fn validate_private_directory(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "voice runtime path is not a real directory",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "voice runtime directory is accessible by other users",
        ));
    }
    let current_uid = fs::metadata("/proc/self")?.uid();
    if metadata.uid() != current_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "voice runtime directory is owned by another user",
        ));
    }
    Ok(())
}

async fn create_temp_audio(
    directory: &Path,
    extension: &str,
    body: &[u8],
) -> io::Result<TempAudio> {
    validate_private_directory(directory)?;
    if extension.is_empty()
        || extension.len() > 5
        || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid audio file extension",
        ));
    }

    let (path, file) = create_unique_private_file(directory, extension)?;
    let temp = TempAudio {
        path,
        cleaned: false,
    };
    let mut file = tokio::fs::File::from_std(file);
    file.write_all(body).await?;
    file.flush().await?;
    drop(file);
    Ok(temp)
}

fn create_unique_private_file(
    directory: &Path,
    extension: &str,
) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let token = random_token()?;
        let path = directory.join(format!(".voice-{token}.{extension}"));
        match open_private_new_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique voice upload file",
    ))
}

fn open_private_new_file(path: &Path) -> io::Result<fs::File> {
    // create_new maps to O_CREAT|O_EXCL and rejects existing symlinks,
    // including dangling ones, instead of following them.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn random_token() -> io::Result<String> {
    let mut random = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let mut token = String::with_capacity(random.len() * 2);
    for byte in random {
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

fn configured_cos_bin() -> OsString {
    env::var_os("COS_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from(DEFAULT_COS_BIN))
}

async fn transcribe_audio(path: &Path) -> Result<String, TranscriptionError> {
    let cos_bin = configured_cos_bin();
    transcribe_audio_with_bin(path, &cos_bin, TRANSCRIPTION_TIMEOUT).await
}

async fn transcribe_audio_with_bin(
    path: &Path,
    cos_bin: &OsStr,
    max_duration: Duration,
) -> Result<String, TranscriptionError> {
    let mut command = Command::new(cos_bin);
    command
        .arg("model")
        .arg("transcribe")
        .arg(path)
        .arg("--format")
        .arg("json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(TranscriptionError::Spawn)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        TranscriptionError::Communicate(io::Error::other("stdout pipe was not created"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        TranscriptionError::Communicate(io::Error::other("stderr pipe was not created"))
    })?;

    let communicate = async {
        let (status, stdout, stderr) = tokio::try_join!(
            child.wait(),
            read_bounded(stdout, STDOUT_LIMIT),
            read_bounded(stderr, STDERR_LIMIT),
        )?;
        Ok::<_, io::Error>((status, stdout, stderr))
    };
    let (status, stdout, stderr) = match timeout(max_duration, communicate).await {
        Ok(result) => result.map_err(TranscriptionError::Communicate)?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(TranscriptionError::Timeout);
        }
    };

    if !status.success() {
        return Err(TranscriptionError::ExitFailure {
            code: status.code(),
            stderr_bytes: stderr.bytes.len(),
            stderr_truncated: stderr.truncated,
        });
    }
    if stdout.truncated {
        return Err(TranscriptionError::StdoutTooLarge);
    }
    parse_transcript_json(&stdout.bytes).map_err(TranscriptionError::InvalidResponse)
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(CapturedOutput { bytes, truncated })
}

fn parse_transcript_json(stdout: &[u8]) -> Result<String, TranscriptParseError> {
    #[derive(Deserialize)]
    struct Transcript {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        result: Option<TranscriptResult>,
    }

    #[derive(Deserialize)]
    struct TranscriptResult {
        #[serde(default)]
        text: Option<String>,
    }

    let response: Transcript =
        serde_json::from_slice(stdout).map_err(|_| TranscriptParseError::InvalidJson)?;
    response
        .text
        .or_else(|| response.result.and_then(|result| result.text))
        .ok_or(TranscriptParseError::MissingText)
}

fn map_transcription_error(error: TranscriptionError) -> VoiceApiError {
    match &error {
        TranscriptionError::Spawn(source) => {
            tracing::warn!(error = %source, "failed to start voice transcription command");
            VoiceApiError::BackendUnavailable
        }
        TranscriptionError::Communicate(source) => {
            tracing::warn!(error = %source, "failed to read voice transcription command");
            VoiceApiError::BackendFailed
        }
        TranscriptionError::Timeout => {
            tracing::warn!("voice transcription command timed out");
            VoiceApiError::BackendTimeout
        }
        TranscriptionError::StdoutTooLarge => {
            tracing::warn!("voice transcription command exceeded its output limit");
            VoiceApiError::InvalidBackendResponse
        }
        TranscriptionError::ExitFailure {
            code,
            stderr_bytes,
            stderr_truncated,
        } => {
            tracing::warn!(
                ?code,
                stderr_bytes,
                stderr_truncated,
                "voice transcription command exited unsuccessfully"
            );
            VoiceApiError::BackendFailed
        }
        TranscriptionError::InvalidResponse(reason) => {
            tracing::warn!(?reason, "voice transcription command returned invalid JSON");
            VoiceApiError::InvalidBackendResponse
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/voice.rs"
    ));
}

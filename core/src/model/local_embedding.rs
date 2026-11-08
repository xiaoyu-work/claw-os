//! Fixed local inference pipe for the installed file indexer and Root broker.

use std::io::{BufRead, Write};

use claw_embed::{local::*, EmbedRequest, Embedder};

pub fn serve() -> Result<(), String> {
    authorize_parent()?;
    let config = crate::config::EmbedConfig::default();
    let embedder = super::tasks::qwen3_genai::build_from_config(&config)
        .with_max_input_tokens(MAX_INPUT_TOKENS)
        .map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut frame = Vec::new();
        loop {
            let bytes = input.fill_buf().map_err(|e| e.to_string())?;
            if bytes.is_empty() {
                if frame.is_empty() {
                    return Ok(());
                }
                return Err("local embedding pipe ended in a partial request".into());
            }
            let take = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|n| n + 1)
                .unwrap_or(bytes.len());
            if frame.len() + take > MAX_PIPE_BYTES {
                return Err("local embedding request is oversized".into());
            }
            let done = bytes[take - 1] == b'\n';
            frame.extend_from_slice(&bytes[..take]);
            input.consume(take);
            if done {
                break;
            }
        }
        let reply = match serde_json::from_slice::<LocalFrame<LocalRequest>>(&frame)
            .map_err(|e| e.to_string())
            .and_then(LocalFrame::into_message)
            .and_then(|request| {
                request.validate()?;
                Ok(request)
            }) {
            Ok(LocalRequest::Status {}) => {
                let check = super::tasks::qwen3_genai::precheck(&config);
                LocalReply::Status {
                    status: LocalStatus {
                        model: LOCAL_MODEL.into(),
                        dim: LOCAL_DIM,
                        configured: check.is_ok(),
                        detail: check.err(),
                    },
                }
            }
            Ok(LocalRequest::Embed { inputs }) => {
                match runtime.block_on(embedder.embed(EmbedRequest { inputs })) {
                    Ok(response) => LocalReply::Embedded { response },
                    Err(error) => LocalReply::Error {
                        message: error.to_string(),
                    },
                }
            }
            Err(message) => LocalReply::Error {
                message: format!("invalid local embedding request: {message}"),
            },
        };
        let bytes = serde_json::to_vec(&LocalFrame::new(reply)).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_PIPE_BYTES {
            return Err("local embedding reply is oversized".into());
        }
        output.write_all(&bytes).map_err(|e| e.to_string())?;
        output.write_all(b"\n").map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
    }
}

fn authorize_parent() -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if unsafe { libc::geteuid() } == 0 {
        return Err("local embedding inference must run as its non-root owner".into());
    }
    let parent = unsafe { libc::getppid() };
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) } != 0
        || unsafe { libc::getppid() } != parent
    {
        return Err("local embedding caller disappeared".into());
    }
    let parent_metadata =
        std::fs::metadata(format!("/proc/{parent}")).map_err(|e| e.to_string())?;
    // The owner cannot inspect a Root process's executable through procfs.
    // Kernel Root parentage authorizes only this fixed, unprivileged CPU pipe.
    if parent_metadata.uid() == 0 {
        return Ok(());
    }
    let executable = std::fs::read_link(format!("/proc/{parent}/exe"))
        .map_err(|e| format!("verify embedding caller: {e}"))?;
    if ![
        "/usr/local/bin/claw-semantic",
        "/usr/local/bin/claw-semantic-daemon",
    ]
    .iter()
    .any(|path| executable == std::path::Path::new(path))
    {
        return Err(
            "local embedding pipe is reserved for the installed indexer and Root broker".into(),
        );
    }
    let metadata = std::fs::symlink_metadata(&executable).map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.file_type().is_symlink()
    {
        return Err("local embedding caller executable is not protected and Root-owned".into());
    }
    if crate::caps::enforcement::process_has_no_new_privs()
        || crate::proc::current_session_info_for_caps()
            .is_some_and(|session| session.app_id.is_some())
    {
        return Err(
            "App/worker descendants cannot invoke the owner file-index embedding pipe".into(),
        );
    }
    Ok(())
}

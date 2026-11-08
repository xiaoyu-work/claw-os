//! Root-derived context exchanges, with an independent disclosure budget.

use super::protocol::{ContextReply, ContextRequest};
use crate::agent::service::{Job, Store};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) async fn read(
    used: &mut u32,
    store: &Store,
    job: &Job,
    request: &ContextRequest,
    shutdown: &AtomicBool,
    deadline: Instant,
    cancelled: bool,
) -> Result<ContextReply, String> {
    *used = used.saturating_add(1);
    if *used > crate::workspaces::gateway::MAX_EXCHANGES || cancelled {
        return Ok(ContextReply::Refused {
            message: "task context is cancelled or its exchange budget is exhausted".into(),
        });
    }
    let read = crate::clawd::workspaces::reads::for_job(job, request.operation.clone());
    tokio::pin!(read);
    let result = loop {
        tokio::select! {
            value = &mut read => break value,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if shutdown.load(Ordering::SeqCst) || store.cancellation_requested(&job.id).unwrap_or(true)
                    || Instant::now() > deadline {
                    break Err("task context read was cancelled or its lease expired".into());
                }
            }
        }
    };
    match result {
        Ok(value) => {
            crate::workspaces::association::check_job(job)?;
            if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len()
                >= crate::workspaces::gateway::MAX_REPLY_BYTES - 1024
            {
                return Ok(ContextReply::Refused {
                    message: "context result exceeds its disclosure budget; narrow the read".into(),
                });
            }
            store
                .append_stream_progress(
                    &job.id,
                    serde_json::json!({
                        "kind":"context_read","workspace_id":job.workspace_id,
                        "request":request.operation,"response":value,
                    }),
                )
                .map_err(|error| {
                    format!("could not record Root context source snapshot: {error}")
                })?;
            Ok(ContextReply::Data { value })
        }
        Err(message) => Ok(ContextReply::Refused { message }),
    }
}

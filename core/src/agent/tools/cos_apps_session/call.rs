//! Serialize each App call with its transient authority and exact process lifetime.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::OwnedMutexGuard;

use crate::agent::tools::mcp::client::McpClient;
use crate::bridge::{AppSessionControl, HostedSessionCall, TransientCall};
use crate::provenance::runtime::ProcessIdentity;

use super::{close_matching_session_at, manager, session_key, ToolResult};

enum CallAuthority {
    Hosted(HostedSessionCall),
    Direct(AppSessionControl),
}

pub(super) struct ActiveCallGuard {
    pub(super) client: Arc<McpClient>,
    pub(super) args: BTreeMap<String, Value>,
    pub(super) session_id: String,
    pub(super) package_digest: String,
    authority: Option<CallAuthority>,
    process: ProcessIdentity,
    completed: bool,
    poisoned: Arc<AtomicBool>,
    _lock: OwnedMutexGuard<()>,
}

impl ActiveCallGuard {
    pub(super) fn finish(mut self, response_received: bool) -> Result<(), String> {
        let result = self.clear();
        self.completed = response_received && result.is_ok();
        result
    }

    fn clear(&mut self) -> Result<(), String> {
        match self.authority.take() {
            Some(CallAuthority::Hosted(call)) => call.finish(),
            Some(CallAuthority::Direct(control)) => control.set_transient_call(None),
            None => Ok(()),
        }
    }
}

impl Drop for ActiveCallGuard {
    fn drop(&mut self) {
        let clear = self.clear();
        if let Err(error) = &clear {
            tracing::error!(
                session = %self.session_id, error = %error,
                "failed to clear App call authority; retiring session"
            );
        }
        if !self.completed || clear.is_err() {
            self.poisoned.store(true, Ordering::SeqCst);
            crate::provenance::runtime::terminate_process_identity(&self.process, Duration::ZERO);
        }
    }
}

pub(super) async fn begin(
    app_id: &str,
    apps_root: &std::path::Path,
    tool: &str,
    args: &BTreeMap<String, Value>,
    caps: &[crate::caps::Cap],
) -> Result<ActiveCallGuard, String> {
    let key = session_key(app_id, apps_root)?;
    let (control, process, call_lock, poisoned, session_id, bound, client) = {
        let table = manager().lock().await;
        let session = table
            .get(&key)
            .ok_or_else(|| format!("App session `{app_id}` is not open"))?;
        (
            session.identity.control(),
            session.process_identity.clone(),
            session.call_lock.clone(),
            session.poisoned.clone(),
            session.identity.id().to_string(),
            Arc::clone(&session.bound),
            session.client.clone(),
        )
    };
    let lock = call_lock.lock_owned().await;
    let prepared = (|| {
        if poisoned.load(Ordering::SeqCst) || !process.still_matches() {
            return Err(format!(
                "App session `{app_id}` was closed before this call"
            ));
        }
        bound.assert_live(&session_id)?;
        if crate::clawd::client::has_gateway() {
            let call = tokio::task::block_in_place(|| control.begin_hosted_call(tool, args))?;
            let canonical = call.prepared.args.clone();
            Ok((CallAuthority::Hosted(call), canonical))
        } else {
            control.set_transient_call(Some(TransientCall {
                tool,
                args,
                caps: crate::caps::CapSet::from_caps(caps.iter().cloned()),
            }))?;
            Ok((CallAuthority::Direct(control), args.clone()))
        }
    })();
    let (authority, args) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            poisoned.store(true, Ordering::SeqCst);
            close_matching_session_at(app_id, apps_root, Some(&session_id)).await;
            return Err(error);
        }
    };
    Ok(ActiveCallGuard {
        client,
        args,
        session_id,
        package_digest: bound.package.content_digest.clone(),
        authority: Some(authority),
        process,
        completed: false,
        poisoned,
        _lock: lock,
    })
}

pub(super) async fn report(
    app_id: &str,
    tool: &str,
    package_digest: &str,
    result: Result<(String, bool), String>,
    cleanup_error: Option<String>,
) -> ToolResult {
    use crate::operations::{receipts, reporting};

    let report = reporting::current().map(|recorder| {
        (
            recorder,
            receipts::capture_session(app_id, tool, package_digest, result.clone()),
        )
    });
    let mut output = match result {
        Ok((content, is_error)) => ToolResult { content, is_error },
        Err(error) => ToolResult::err(error),
    };
    if let Some(error) = cleanup_error {
        output.content.push_str(&format!(
            "\n\nApp session was retired because call authority could not be cleared: {}. Do not repeat the call automatically.",
            receipts::diagnostic(&error),
        ));
        output.is_error = true;
    }
    match report {
        Some((recorder, report)) => {
            super::super::app_receipts::deliver(output, recorder, report).await
        }
        None => output,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_apps_session/call.rs"
    ));
}

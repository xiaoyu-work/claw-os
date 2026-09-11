//! Stateful call authority and stable task-local aliases for rotated grants.

use std::collections::BTreeMap;

use super::*;
use crate::operations::invocation::{AppSessionInvocation, PreparedSessionCall};

pub(super) struct StatefulSession {
    pub(super) launch_alias: String,
    launch_handle: String,
    pub(super) relay_alias: Option<String>,
    relay_handle: Option<String>,
    active: Option<ActiveCall>,
    last_ended: Option<String>,
}

struct ActiveCall {
    id: String,
    _tool: String,
    _original_args: BTreeMap<String, Value>,
}

impl StatefulSession {
    pub(super) fn new(launch_handle: String) -> Self {
        Self {
            launch_alias: uuid::Uuid::new_v4().simple().to_string(),
            launch_handle,
            relay_alias: None,
            relay_handle: None,
            active: None,
            last_ended: None,
        }
    }

    pub(super) fn install_initial_relay(&mut self, handle: String) {
        self.relay_handle = Some(handle);
        self.relay_alias = Some(uuid::Uuid::new_v4().simple().to_string());
    }
}

impl AppHost {
    pub(super) fn resolve_launch_grant(
        &self,
        session_id: &str,
        app_id: &str,
        handle: &str,
    ) -> Result<crate::clawd::authority::GrantId, BrokerError> {
        use crate::clawd::authority::{authority, Audience, Presentation};
        let uid = self
            .client
            .require_uid()
            .map_err(BrokerError::authorization)?;
        let pid = self
            .client
            .pid
            .ok_or_else(|| BrokerError::authorization("App host has no process"))?;
        let view = authority()
            .resolve(
                handle,
                &Presentation::new(
                    uid,
                    pid,
                    self.client.start_time_ticks,
                    Audience::AppLaunch,
                    "app_host.control",
                ),
            )
            .map_err(|error| BrokerError::authorization(error.to_string()))?;
        if view.subject.session_id.as_deref() != Some(session_id)
            || view.subject.app_id.as_deref() != Some(app_id)
        {
            return Err(BrokerError::authorization(
                "rotated launch grant belongs to another App session",
            ));
        }
        Ok(view.id)
    }

    pub(super) fn prepare_session(
        &self,
        original: AppSessionInvocation,
        sessions: &mut Sessions,
    ) -> Result<PreparedInvocation, BrokerError> {
        if sessions.invocations.len() >= MAX_ACTIVE_SESSIONS {
            return Err(BrokerError::authorization("too many retained App sessions"));
        }
        let parent = self
            .parent
            .as_ref()
            .ok_or_else(|| BrokerError::authorization("task has no capability session"))?;
        let session = crate::clawd::app_sessions::TaskHostSession {
            app_id: &original.app_id,
            package_digest: &original.package_digest,
        };
        crate::clawd::app_sessions::prepare_session_for_task_host(&self.client, parent, &session)?;
        let root = std::env::var_os("COS_APPS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/usr/lib/cos/apps".into());
        let app =
            crate::apps::find_verified(&root, &original.app_id).map_err(BrokerError::execution)?;
        let package = app
            .require_verified()
            .map_err(BrokerError::execution)?
            .clone();
        if package.content_digest() != original.package_digest {
            return Err(BrokerError::authorization(
                "App package changed during session preparation",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        sessions.invocations.insert(
            id.clone(),
            Invocation {
                original: OriginalInvocation::Session(original),
                package,
            },
        );
        Ok(PreparedInvocation {
            id,
            args: Vec::new(),
        })
    }

    pub(super) fn translate_session_handle(
        &self,
        call: &AppHostCall,
        params: &mut Value,
        sessions: &Sessions,
    ) -> Result<(), BrokerError> {
        let Some(id) = call.session_id() else {
            return Ok(());
        };
        let hosted = sessions
            .active
            .get(id)
            .ok_or_else(|| BrokerError::authorization("App session is not hosted by this task"))?;
        let Some(state) = &hosted.stateful else {
            if matches!(call, AppHostCall::StartCall(_) | AppHostCall::EndCall(_)) {
                return Err(BrokerError::authorization(
                    "one-shot App sessions cannot acquire stateful call authority",
                ));
            }
            return Ok(());
        };
        match call {
            AppHostCall::StartCall(_) if hosted.process.is_none() => {
                return Err(BrokerError::authorization(
                    "App session is not bound to a process",
                ));
            }
            AppHostCall::StartCall(_) if state.active.is_some() => {
                return Err(BrokerError::authorization(
                    "another call is already active for this App",
                ));
            }
            AppHostCall::EndCall(end)
                if state.active.as_ref().map(|active| active.id.as_str())
                    != Some(end.call_id.as_str())
                    && !(state.active.is_none()
                        && state.last_ended.as_deref() == Some(end.call_id.as_str())) =>
            {
                return Err(BrokerError::authorization(
                    "call-end identity does not match the active App call",
                ));
            }
            _ => {}
        }
        let (alias, handle) = match call {
            AppHostCall::Relay(_) => {
                if state.active.is_none() {
                    return Err(BrokerError::authorization(
                        "stateful App has no active authorized call",
                    ));
                }
                (state.relay_alias.as_deref(), state.relay_handle.as_deref())
            }
            AppHostCall::Bind(_)
            | AppHostCall::Deregister(_)
            | AppHostCall::StartCall(_)
            | AppHostCall::EndCall(_) => (
                Some(state.launch_alias.as_str()),
                Some(state.launch_handle.as_str()),
            ),
            _ => return Ok(()),
        };
        if alias.is_none() || params.get("handle").and_then(Value::as_str) != alias {
            return Err(BrokerError::authorization(
                "App control alias does not belong to this task session",
            ));
        }
        params["handle"] = Value::String(
            handle
                .ok_or_else(|| BrokerError::authorization("App control has no live grant"))?
                .to_string(),
        );
        Ok(())
    }

    pub(super) async fn session_call(
        &self,
        call: &AppHostCall,
        mut params: Value,
        sessions: &mut Sessions,
    ) -> Result<Value, BrokerError> {
        let id = call
            .session_id()
            .ok_or_else(|| BrokerError::execution("stateful call has no session"))?;
        let hosted = sessions
            .active
            .get_mut(id)
            .ok_or_else(|| BrokerError::authorization("unknown hosted session"))?;
        let state = hosted
            .stateful
            .as_mut()
            .ok_or_else(|| BrokerError::authorization("App is not a stateful session"))?;
        let parent = self
            .parent
            .as_ref()
            .ok_or_else(|| BrokerError::authorization("task has no capability session"))?;
        let specification = crate::clawd::app_sessions::TaskHostSession {
            app_id: hosted.package.id(),
            package_digest: hosted.package.content_digest(),
        };
        let original = match call {
            AppHostCall::StartCall(_) => {
                if state.active.is_some() {
                    return Err(BrokerError::authorization(
                        "another call is already active for this App",
                    ));
                }
                let tool = params
                    .pointer("/call/tool")
                    .and_then(Value::as_str)
                    .ok_or_else(|| BrokerError::execution("session tool name is required"))?
                    .to_string();
                let args: BTreeMap<String, Value> = serde_json::from_value(
                    params
                        .pointer("/call/args")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                )
                .map_err(|error| {
                    BrokerError::execution(format!("session call args must be an object: {error}"))
                })?;
                Some((tool, args))
            }
            AppHostCall::EndCall(end) => {
                if state.active.is_none()
                    && state.last_ended.as_deref() == Some(end.call_id.as_str())
                {
                    return Ok(json!({"cleared":true}));
                }
                if state.active.as_ref().map(|active| active.id.as_str())
                    != Some(end.call_id.as_str())
                {
                    return Err(BrokerError::authorization(
                        "call-end identity does not match the active App call",
                    ));
                }
                None
            }
            _ => return Err(BrokerError::execution("invalid stateful call control")),
        };
        let original_call = original
            .as_ref()
            .map(|(tool, args)| crate::clawd::app_sessions::TaskHostSessionCall { tool, args });
        let canonical = if let Some(original) = &original_call {
            let args = crate::clawd::app_sessions::prepare_session_call_for_task_host(
                &self.client,
                parent,
                &specification,
                original,
            )?;
            params["call"]["args"] = serde_json::to_value(&args)
                .map_err(|error| BrokerError::execution(error.to_string()))?;
            Some(args)
        } else {
            None
        };
        let result = crate::clawd::app_sessions::set_session_call_for_task_host(
            params,
            &self.client,
            parent,
            &specification,
            original_call.as_ref(),
        )
        .await?;
        if result.get("updated").and_then(Value::as_bool) != Some(true) {
            return Err(BrokerError::unavailable(
                "stateful transition was not acknowledged",
            ));
        }
        let launch = result
            .get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrokerError::unavailable("stateful transition omitted its launch handle")
            })?
            .to_string();
        let relay = result
            .get("relay_handle")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrokerError::unavailable("stateful transition omitted its relay handle")
            })?
            .to_string();
        hosted.launch_grant = self.resolve_launch_grant(id, hosted.package.id(), &launch)?;
        state.launch_handle = launch;
        state.relay_handle = Some(relay);
        match (original, canonical) {
            (Some((tool, args)), Some(canonical)) => {
                let id = uuid::Uuid::new_v4().to_string();
                state.active = Some(ActiveCall {
                    id: id.clone(),
                    _tool: tool,
                    _original_args: args,
                });
                serde_json::to_value(PreparedSessionCall {
                    id,
                    args: canonical,
                })
                .map_err(|error| BrokerError::execution(error.to_string()))
            }
            (None, None) => {
                state.last_ended = state.active.take().map(|active| active.id);
                Ok(json!({"cleared":true}))
            }
            _ => Err(BrokerError::execution(
                "stateful call preparation is inconsistent",
            )),
        }
    }
}

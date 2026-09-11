//! Root-side App control for one authenticated task, not a general RPC proxy.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use serde_json::{json, Value};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::{BrokerError, BrokerErrorKind, Request, Response};
use crate::clawd::state::DaemonState;
use crate::clawd::transport::limits::Admission;
use crate::operations::invocation::{AppInvocation, AppSessionInvocation, PreparedInvocation};
use crate::proc::SessionInfo;
use crate::provenance::runtime::{InstanceClass, PackageRef, ProcessIdentity};
use crate::provenance::VerifiedPackage;

use super::protocol::{
    AppHostCall, AppHostRequest, MAX_ACTIVE_CALLS, MAX_ACTIVE_SESSIONS, MAX_CONTROL_CALLS,
};

mod stateful;
use stateful::StatefulSession;

struct HostedSession {
    package: Arc<VerifiedPackage>,
    invocation_id: String,
    process: Option<ProcessIdentity>,
    launch_grant: crate::clawd::authority::GrantId,
    stateful: Option<StatefulSession>,
}

enum OriginalInvocation {
    Operation(AppInvocation),
    Session(AppSessionInvocation),
}

struct Invocation {
    original: OriginalInvocation,
    package: Arc<VerifiedPackage>,
}

#[derive(Default)]
struct Sessions {
    invocations: HashMap<String, Invocation>,
    active: HashMap<String, HostedSession>,
    approvals: HashSet<String>,
}

pub(crate) struct AppHost {
    task_id: String,
    client: ClientIdentity,
    parent: Option<SessionInfo>,
    state: DaemonState,
    admission: Arc<Admission>,
    sessions: Mutex<Sessions>,
    deadline: StdMutex<Instant>,
    closed: AtomicBool,
    used: AtomicU32,
    permits: Arc<Semaphore>,
}

pub(crate) struct HostLifetime(Arc<AppHost>);

impl Drop for HostLifetime {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/broker.rs"
    ));
}

fn verify_instance(uid: u32, id: &str, hosted: &HostedSession) -> Result<(), String> {
    let instance = crate::provenance::runtime::instance_for(uid, id)?
        .ok_or_else(|| "App runtime registration was not persisted".to_string())?;
    if instance.class != InstanceClass::App
        || instance.package.as_ref() != Some(&PackageRef::of(&hosted.package))
        || instance.process != hosted.process
    {
        return Err(
            "App runtime package or process binding does not match the hosted session".to_string(),
        );
    }
    Ok(())
}

fn retire_session(uid: u32, id: &str, hosted: &HostedSession) -> Result<(), String> {
    crate::clawd::authority::authority().revoke(hosted.launch_grant);
    crate::clawd::authority::revoke_session_for_owner(id, uid);
    if let Some(process) = &hosted.process {
        crate::provenance::runtime::terminate_process_identity(
            process,
            std::time::Duration::from_millis(100),
        );
        if process.still_matches() {
            return Err(
                "App child is still live after identity-checked termination; records retained"
                    .to_string(),
            );
        }
    }
    if crate::provenance::runtime::instance_for(uid, id)?.is_some() {
        verify_instance(uid, id, hosted)?;
        crate::provenance::runtime::deregister(uid, id);
        if crate::provenance::runtime::instance_for(uid, id)?.is_some() {
            return Err("App runtime record could not be retired".to_string());
        }
    }
    crate::proc::try_deregister_session_for_owner(id, uid)?;
    Ok(())
}

impl AppHost {
    pub(crate) fn new(
        task_id: String,
        client: ClientIdentity,
        parent: Option<SessionInfo>,
        state: DaemonState,
        admission: Arc<Admission>,
        deadline: Instant,
    ) -> Arc<Self> {
        Arc::new(Self {
            task_id,
            client,
            parent,
            state,
            admission,
            sessions: Mutex::new(Sessions::default()),
            deadline: StdMutex::new(deadline),
            closed: AtomicBool::new(false),
            used: AtomicU32::new(0),
            permits: Arc::new(Semaphore::new(MAX_ACTIVE_CALLS)),
        })
    }

    pub(crate) fn lifetime(self: &Arc<Self>) -> HostLifetime {
        HostLifetime(self.clone())
    }

    pub(crate) fn renew(&self, deadline: Instant) -> Result<(), String> {
        *self
            .deadline
            .lock()
            .map_err(|_| "App-host lease lock is poisoned".to_string())? = deadline;
        Ok(())
    }

    pub(crate) fn reserve(&self) -> Result<OwnedSemaphorePermit, String> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| "too many in-flight App-host requests".to_string())
    }

    fn check_lease(&self, task_id: &str) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) || self.task_id != task_id {
            return Err("App host is closed or belongs to another task".to_string());
        }
        if Instant::now()
            > *self
                .deadline
                .lock()
                .map_err(|_| "App-host lease lock is poisoned".to_string())?
        {
            return Err("App-host task lease has expired".to_string());
        }
        let pid = self
            .client
            .pid
            .ok_or_else(|| "App-host process is unavailable".to_string())?;
        if !crate::proc::is_pid_alive(pid)
            || crate::proc::read_start_time_ticks_pub(pid) != self.client.start_time_ticks
        {
            return Err("App-host process identity is no longer current".to_string());
        }
        Ok(())
    }

    pub(crate) async fn handle(&self, request: AppHostRequest) -> Response {
        let id = request.request_id.clone();
        let result = self.handle_inner(request).await;
        match result {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    task = %self.task_id,
                    code = error.kind.code(),
                    "controlled App-host request refused"
                );
                Response::handler_error(id, error)
            }
        }
    }

    async fn handle_inner(&self, request: AppHostRequest) -> Result<Response, BrokerError> {
        self.check_lease(&request.task_id)
            .map_err(BrokerError::authorization)?;
        request.call.validate().map_err(BrokerError::execution)?;
        if self
            .used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                (used < MAX_CONTROL_CALLS).then_some(used.saturating_add(1))
            })
            .is_err()
        {
            return Err(BrokerError::authorization(
                "App-host control budget exhausted",
            ));
        }
        let mut sessions = self.sessions.lock().await;
        self.check_lease(&request.task_id)
            .map_err(BrokerError::authorization)?;
        let uid = self
            .client
            .require_uid()
            .map_err(BrokerError::authorization)?;
        let _private_permit = if request.call.command().is_none() {
            Some(
                self.admission
                    .accept_request(uid)
                    .map_err(BrokerError::fault)?,
            )
        } else {
            None
        };
        if request.call.command().is_none() {
            match &request.call {
                AppHostCall::Begin(invocation) => {
                    let prepared = self.prepare(invocation.clone(), &mut sessions)?;
                    return Ok(Response::ok(
                        request.request_id,
                        serde_json::to_value(prepared)
                            .map_err(|error| BrokerError::execution(error.to_string()))?,
                    ));
                }
                AppHostCall::BeginSession(invocation) => {
                    let prepared = self.prepare_session(invocation.clone(), &mut sessions)?;
                    return Ok(Response::ok(
                        request.request_id,
                        serde_json::to_value(prepared)
                            .map_err(|error| BrokerError::execution(error.to_string()))?,
                    ));
                }
                AppHostCall::End(end) => {
                    let invocation_id = end.invocation_id.as_str();
                    if !sessions.invocations.contains_key(invocation_id) {
                        return Err(BrokerError::authorization(
                            "invocation is not owned by this task",
                        ));
                    }
                    let owned: Vec<_> = sessions
                        .active
                        .iter()
                        .filter(|(_, session)| session.invocation_id == invocation_id)
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in owned {
                        let session = sessions.active.remove(&id).expect("owned session");
                        retire_session(uid, &id, &session).map_err(BrokerError::unavailable)?;
                    }
                    sessions.invocations.remove(invocation_id);
                    return Ok(Response::ok(request.request_id, json!({"retired":true})));
                }
                _ => {}
            }
        }
        if let Some(session) = request.call.session_id() {
            if !sessions.active.contains_key(session) {
                return Err(BrokerError::authorization(
                    "App session is not hosted by this task",
                ));
            }
        }
        if let AppHostCall::Check(check) = &request.call {
            let hosted = &sessions.active[check.session_id.as_str()];
            if hosted.process.is_none() || hosted.package.content_digest() != check.package_digest {
                return Err(BrokerError::authorization(
                    "App instance is not bound to this package",
                ));
            }
            hosted
                .package
                .assert_current(&crate::provenance::trust_store())
                .map_err(|error| BrokerError::authorization(error.to_string()))?;
            crate::provenance::runtime::assert_live_instance_now(uid, check.session_id.as_str())
                .map_err(BrokerError::authorization)?;
            verify_instance(uid, check.session_id.as_str(), hosted)
                .map_err(BrokerError::authorization)?;
            let process = hosted.process.as_ref().expect("bound App process");
            let grant = crate::clawd::authority::authority()
                .resolve_session(
                    check.session_id.as_str(),
                    &crate::clawd::authority::Presentation::new(
                        uid,
                        process.pid,
                        process.start_time_ticks,
                        crate::clawd::authority::Audience::SystemService,
                        "app_host.check",
                    ),
                )
                .map_err(|error| BrokerError::authorization(error.to_string()))?;
            return Ok(Response::ok(
                request.request_id,
                json!({"live":true,"caps":grant.caps}),
            ));
        }
        let binding = if let AppHostCall::Bind(bind) = &request.call {
            let hosted = &sessions.active[bind.session_id.as_str()];
            hosted
                .package
                .assert_current(&crate::provenance::trust_store())
                .map_err(|error| BrokerError::authorization(error.to_string()))?;
            hosted
                .package
                .manifest_text()
                .map_err(|error| BrokerError::authorization(error.to_string()))?;
            verify_instance(uid, bind.session_id.as_str(), hosted)
                .map_err(BrokerError::authorization)?;
            Some(
                ProcessIdentity::of_process(uid, bind.pid)
                    .filter(ProcessIdentity::still_matches)
                    .ok_or_else(|| {
                        BrokerError::authorization("App child cannot be identified before bind")
                    })?,
            )
        } else {
            None
        };
        if let AppHostCall::ApprovalStatus(status) = &request.call {
            let params = serde_json::to_value(status)
                .map_err(|error| BrokerError::execution(error.to_string()))?;
            let ids = params["ids"]
                .as_array()
                .ok_or_else(|| BrokerError::execution("missing approval identifiers"))?;
            if ids.iter().any(|id| {
                id.as_str()
                    .is_none_or(|id| !sessions.approvals.contains(id))
            }) {
                return Err(BrokerError::authorization(
                    "approval does not belong to this App host",
                ));
            }
        }
        let command = request
            .call
            .command()
            .ok_or_else(|| BrokerError::execution("unknown App-host method"))?;
        let mut params = request.call.params().map_err(BrokerError::execution)?;
        self.translate_session_handle(&request.call, &mut params, &sessions)?;
        let wire = Request {
            v: crate::clawd::wire::PROTOCOL_VERSION,
            id: request.request_id.clone(),
            command,
            params,
        };
        let encoded = crate::clawd::protocol::encode_request(&wire)
            .map_err(|error| BrokerError::execution(error.to_string()))?;
        let admitted =
            match crate::clawd::server::admit(&encoded, &self.client, &self.admission).await {
                Ok(admitted) => admitted,
                Err(refusal) => return Ok(Response::fault(refusal.id, refusal.fault)),
            };
        let facts = crate::audit_policy::request_facts_for_route(
            admitted.route.name,
            admitted.route.audit_fields,
            &admitted.params,
        );
        let started = Instant::now();
        let bracket = crate::clawd::journal::begin(
            admitted.route,
            &admitted.id,
            admitted.decision.as_ref(),
            &self.client,
        )
        .map_err(BrokerError::fault)?;
        let mut response = match &request.call {
            AppHostCall::Register(registration) => {
                let result = self
                    .register(registration, admitted.params.clone(), &mut sessions)
                    .await;
                match result {
                    Ok(value) => Response::ok(admitted.id.clone(), value),
                    Err(error) => Response::handler_error(admitted.id.clone(), error),
                }
            }
            AppHostCall::StartCall(_) | AppHostCall::EndCall(_) => {
                match self
                    .session_call(&request.call, admitted.params.clone(), &mut sessions)
                    .await
                {
                    Ok(value) => Response::ok(admitted.id.clone(), value),
                    Err(error) => Response::handler_error(admitted.id.clone(), error),
                }
            }
            _ => {
                crate::clawd::server::dispatch(
                    admitted.route,
                    admitted.id.clone(),
                    admitted.params.clone(),
                    admitted.decision.as_ref(),
                    &self.state,
                    &self.client,
                )
                .await
            }
        };
        if response.ok {
            if let Err(error) =
                self.after_dispatch(&request.call, uid, binding, &mut sessions, &mut response)
            {
                if let Some(id) = request.call.session_id() {
                    if let Some(session) = sessions.active.remove(id) {
                        if let Err(cleanup) = retire_session(uid, id, &session) {
                            tracing::error!(error = %cleanup, "failed to retire an incompletely bound App");
                        }
                    }
                }
                response = Response::handler_error(
                    admitted.id.clone(),
                    BrokerError {
                        kind: BrokerErrorKind::Indeterminate,
                        message: error,
                        data: None,
                        audit_class: Some("app_host_binding_indeterminate"),
                    },
                );
            }
        } else if let Some(error) = &response.error {
            let approval_required = error
                .data
                .as_ref()
                .and_then(|data| data.get("status"))
                .and_then(Value::as_str)
                == Some("approval_required");
            if matches!(
                request.call,
                AppHostCall::StartCall(_) | AppHostCall::EndCall(_)
            ) && !approval_required
            {
                if let Some(id) = request.call.session_id() {
                    if let Some(session) = sessions.active.remove(id) {
                        if let Err(cleanup) = retire_session(uid, id, &session) {
                            tracing::error!(error = %cleanup, "failed to retire a failed stateful App call");
                        }
                    }
                }
            }
            if let Some(ids) = error
                .data
                .as_ref()
                .and_then(|data| data.get("approval_requests"))
                .and_then(Value::as_array)
            {
                for id in ids.iter().filter_map(Value::as_str) {
                    if sessions.approvals.len() < MAX_CONTROL_CALLS as usize {
                        sessions.approvals.insert(id.to_string());
                    }
                }
            }
        }
        if let Some(bracket) = bracket {
            if let Some(replacement) =
                crate::clawd::journal::finish(bracket, &admitted.id, &response)
            {
                response = replacement;
            }
        }
        let outcome = response.audit_facts();
        if let Err(error) =
            crate::clawd::audit::record_request(&facts, &outcome, started.elapsed(), &self.client)
        {
            tracing::error!(error = %error, "failed to record App-host broker audit");
        }
        crate::clawd::system_journal::record_clawd_request(
            &facts,
            &outcome,
            started.elapsed(),
            &self.client,
        );
        Ok(response)
    }

    async fn register(
        &self,
        registration: &super::protocol::Registration,
        params: Value,
        sessions: &mut Sessions,
    ) -> Result<Value, BrokerError> {
        if sessions.active.len() >= MAX_ACTIVE_SESSIONS {
            return Err(BrokerError::authorization(
                "too many active App sessions for this task",
            ));
        }
        let parent = self
            .parent
            .as_ref()
            .ok_or_else(|| BrokerError::authorization("task has no capability session"))?;
        let retained = sessions
            .invocations
            .get(registration.invocation_id.as_str())
            .ok_or_else(|| {
                BrokerError::authorization("App registration has no retained invocation")
            })?;
        let package = retained.package.clone();
        let mut value = match &retained.original {
            OriginalInvocation::Operation(original) => {
                let invocation = crate::clawd::app_sessions::TaskHostInvocation {
                    app_id: &original.app_id,
                    operation: &original.operation,
                    args: &original.args,
                    package_digest: &original.package_digest,
                };
                crate::clawd::app_sessions::register_for_task_host(
                    params,
                    &self.client,
                    parent,
                    &invocation,
                )
                .await?
            }
            OriginalInvocation::Session(original) => {
                let session = crate::clawd::app_sessions::TaskHostSession {
                    app_id: &original.app_id,
                    package_digest: &original.package_digest,
                };
                crate::clawd::app_sessions::register_session_for_task_host(
                    params,
                    &self.client,
                    parent,
                    &session,
                )
                .await?
            }
        };
        let id = value
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| BrokerError::unavailable("App registration omitted its session"))?
            .to_string();
        let handle = value.get("handle").and_then(Value::as_str).ok_or_else(|| {
            BrokerError::unavailable("App registration omitted its launch handle")
        })?;
        let launch_grant = self.resolve_launch_grant(&id, package.id(), handle)?;
        let stateful = if matches!(retained.original, OriginalInvocation::Session(_)) {
            let state = StatefulSession::new(handle.to_string());
            value["handle"] = json!(state.launch_alias);
            Some(state)
        } else {
            None
        };
        sessions.active.insert(
            id.clone(),
            HostedSession {
                package: package.clone(),
                invocation_id: registration.invocation_id.as_str().to_string(),
                process: None,
                launch_grant,
                stateful,
            },
        );
        let uid = self
            .client
            .require_uid()
            .map_err(BrokerError::authorization)?;
        crate::provenance::runtime::register(uid, &id, &package);
        if let Err(error) = verify_instance(uid, &id, &sessions.active[&id]) {
            let session = sessions.active.remove(&id).expect("registered App session");
            if let Err(cleanup) = retire_session(uid, &id, &session) {
                tracing::error!(error = %cleanup, "failed to retire App registration after bookkeeping failure");
            }
            return Err(BrokerError::unavailable(error));
        }
        Ok(value)
    }

    fn prepare(
        &self,
        original: AppInvocation,
        sessions: &mut Sessions,
    ) -> Result<PreparedInvocation, BrokerError> {
        if sessions.invocations.len() >= MAX_ACTIVE_SESSIONS {
            return Err(BrokerError::authorization(
                "too many retained App invocations",
            ));
        }
        let parent = self
            .parent
            .as_ref()
            .ok_or_else(|| BrokerError::authorization("task has no capability session"))?;
        let invocation = crate::clawd::app_sessions::TaskHostInvocation {
            app_id: &original.app_id,
            operation: &original.operation,
            args: &original.args,
            package_digest: &original.package_digest,
        };
        let args =
            crate::clawd::app_sessions::prepare_for_task_host(&self.client, parent, &invocation)?;
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
                "App package changed during invocation preparation",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        sessions.invocations.insert(
            id.clone(),
            Invocation {
                original: OriginalInvocation::Operation(original),
                package,
            },
        );
        Ok(PreparedInvocation { id, args })
    }

    fn after_dispatch(
        &self,
        call: &AppHostCall,
        uid: u32,
        binding: Option<ProcessIdentity>,
        sessions: &mut Sessions,
        response: &mut Response,
    ) -> Result<(), String> {
        match call {
            AppHostCall::Bind(bind) => {
                let hosted = sessions
                    .active
                    .get_mut(bind.session_id.as_str())
                    .ok_or_else(|| "App host lost its registered session".to_string())?;
                hosted.process = Some(
                    binding
                        .ok_or_else(|| "App bind lost its checked child identity".to_string())?,
                );
                crate::provenance::runtime::bind_process(uid, bind.session_id.as_str(), bind.pid);
                verify_instance(uid, bind.session_id.as_str(), hosted)?;
                if let Some(state) = &mut hosted.stateful {
                    let result = response
                        .result
                        .as_mut()
                        .ok_or_else(|| "App bind returned no result".to_string())?;
                    let handle = result
                        .get("relay_handle")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "stateful App bind returned no relay handle".to_string())?;
                    state.install_initial_relay(handle.to_string());
                    result["relay_handle"] = json!(state.relay_alias);
                }
            }
            AppHostCall::Deregister(release) => {
                let session = sessions
                    .active
                    .remove(release.session_id.as_str())
                    .ok_or_else(|| "App deregistration lost its owned session".to_string())?;
                retire_session(uid, release.session_id.as_str(), &session)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn close(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return None;
        }
        let host = self.clone();
        // Do not abort an admitted privileged mutation. Cleanup waits behind
        // the same lock and revokes all remaining sessions once it completes.
        Some(tokio::spawn(async move {
            let mut sessions = host.sessions.lock().await;
            if let Some(uid) = host.client.uid {
                for (id, session) in sessions.active.drain() {
                    if let Err(error) = retire_session(uid, &id, &session) {
                        tracing::error!(error = %error, "failed to retire a task-owned App session");
                    }
                }
                sessions.invocations.clear();
            }
        }))
    }
}

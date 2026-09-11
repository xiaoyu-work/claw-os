use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use claw_display_control::{workload::InstanceProcess, ProcessIdentity};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::{UnixListener, UnixStream};

use crate::clawd::app_sessions::gui::{kernel_peer, Registration};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::{encode_response, BrokerError, Response};
use crate::clawd::routes::Command;
use crate::clawd::state::DaemonState;
use crate::clawd::transport::{peer, Admission, PeerStream, ReadOutcome};
use crate::clawd::wire::{
    bounded::{Text, TextList},
    Fault, InboundRequest, Request, RequestId, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    PROTOCOL_VERSION,
};

const CONNECTIONS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

struct BoundProcess {
    process: ProcessIdentity,
    containment: InstanceProcess,
}

struct State {
    registration: Arc<Registration>,
    process: OnceLock<BoundProcess>,
    stopped: AtomicBool,
    listener_done: AtomicBool,
    inflight: AtomicUsize,
    failure: Mutex<Option<String>>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

pub(super) struct Broker {
    state: Arc<State>,
}

impl Broker {
    pub fn start(
        listener: std::os::unix::net::UnixListener,
        registration: Arc<Registration>,
        daemon: DaemonState,
        admission: Arc<Admission>,
        executor: &tokio::runtime::Handle,
    ) -> Result<Self, String> {
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        peer::enable_credential_passing(listener.as_raw_fd()).map_err(|error| error.to_string())?;
        let _runtime = executor.enter();
        let listener = UnixListener::from_std(listener).map_err(|error| error.to_string())?;
        let (shutdown, mut ended) = tokio::sync::watch::channel(false);
        let state = Arc::new(State {
            registration,
            process: OnceLock::new(),
            stopped: AtomicBool::new(false),
            listener_done: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            failure: Mutex::new(None),
            shutdown,
        });
        let endpoint = state.clone();
        let context = crate::paths::RoutedPathContext::capture();
        executor.spawn(async move {
            let _done = ListenerDone(endpoint.clone());
            loop {
                let accepted = tokio::select! {
                    _ = ended.changed() => return,
                    accepted = listener.accept() => accepted,
                };
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::error!(%error, "GUI broker listener failed");
                        if let Ok(mut failure) = endpoint.failure.lock() {
                            *failure = Some(error.to_string());
                        }
                        return;
                    }
                };
                if endpoint.stopped.load(Ordering::Acquire) {
                    return;
                }
                if endpoint.inflight.load(Ordering::Acquire) >= CONNECTIONS {
                    tracing::warn!("GUI broker connection ceiling reached");
                    continue;
                }
                endpoint.inflight.fetch_add(1, Ordering::AcqRel);
                let endpoint = endpoint.clone();
                let daemon = daemon.clone();
                let admission = admission.clone();
                let context = context.clone();
                tokio::spawn(async move {
                    let _active = Active(endpoint.clone());
                    context
                        .scope(serve(stream, endpoint, daemon, admission))
                        .await;
                });
            }
        });
        Ok(Self { state })
    }

    pub fn bind(
        &self,
        process: ProcessIdentity,
        resources: &crate::worker::LaunchResources,
    ) -> Result<(), String> {
        let containment = InstanceProcess::receive(
            process.pidfd().map_err(|error| error.to_string())?,
            resources.gui_cgroup()?,
        )
        .map_err(|error| error.to_string())?;
        self.state
            .process
            .set(BoundProcess {
                process,
                containment,
            })
            .map_err(|_| "GUI broker process was already bound".to_string())
    }

    pub fn stop(&self) {
        self.state.stopped.store(true, Ordering::Release);
        self.state.shutdown.send_replace(true);
    }

    pub fn quiescent(&self) -> bool {
        self.state.listener_done.load(Ordering::Acquire)
            && self.state.inflight.load(Ordering::Acquire) == 0
    }

    pub fn health(&self) -> Result<(), String> {
        let failure = self
            .state
            .failure
            .lock()
            .map_err(|_| "GUI broker status lock poisoned")?;
        match failure.as_ref() {
            Some(error) => Err(format!("GUI broker unavailable: {error}")),
            None => Ok(()),
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Active(Arc<State>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ListenerDone(Arc<State>);
impl Drop for ListenerDone {
    fn drop(&mut self) {
        self.0.listener_done.store(true, Ordering::Release);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyQuery {
    verb: Text<128>,
    scope: crate::caps::Scope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryQuery {
    command: Text<64>,
    args: TextList<16, 65536>,
}

enum Validated {
    Policy,
    Memory,
    Provider(Request),
}

fn validate(request: &mut InboundRequest, session: &str) -> Result<Validated, Fault> {
    if request.v != PROTOCOL_VERSION {
        return Err(Fault::UnsupportedVersion);
    }
    match request.command.as_str() {
        crate::worker::broker::POLICY_CHECK_COMMAND => {
            let query: PolicyQuery =
                serde_json::from_value(request.params.clone()).map_err(|_| Fault::InvalidParams)?;
            crate::caps::Verb::parse(query.verb.as_str()).ok_or(Fault::InvalidParams)?;
            let _ = query.scope;
            Ok(Validated::Policy)
        }
        crate::worker::broker::MEMORY_CALL_COMMAND => {
            let query: MemoryQuery =
                serde_json::from_value(request.params.clone()).map_err(|_| Fault::InvalidParams)?;
            if !crate::worker::broker::MEMORY_SUBCOMMANDS.contains(&query.command.as_str()) {
                return Err(Fault::InvalidParams);
            }
            let _ = query.args;
            Ok(Validated::Memory)
        }
        name => {
            let command = Command::parse(name).ok_or(Fault::UnknownCommand)?;
            let route = crate::clawd::app_sessions::gui::provider_route(command)
                .map_err(|_| Fault::NotAuthorized)?;
            let params = request.params.as_object_mut().ok_or(Fault::InvalidParams)?;
            if params
                .get("session")
                .is_some_and(|value| value.as_str() != Some(session))
            {
                return Err(Fault::NotAuthorized);
            }
            params.insert("session".to_string(), Value::String(session.to_string()));
            let params = (route.decode)(request.params.clone())?;
            Ok(Validated::Provider(Request {
                v: request.v,
                id: request.id.clone(),
                command,
                params,
            }))
        }
    }
}

async fn serve(
    stream: UnixStream,
    endpoint: Arc<State>,
    daemon: DaemonState,
    admission: Arc<Admission>,
) {
    let mut stream = match PeerStream::new(stream) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "GUI broker cannot authenticate connection");
            return;
        }
    };
    let mut shutdown = endpoint.shutdown.subscribe();
    let read = tokio::select! {
        _ = shutdown.changed() => return,
        read = tokio::time::timeout(IO_TIMEOUT, stream.read_request(MAX_REQUEST_BYTES)) => read,
    };
    let frame = match read {
        Ok(Ok(ReadOutcome::Frame(frame))) => frame,
        Ok(Ok(ReadOutcome::Closed)) => return,
        Ok(Ok(ReadOutcome::Legacy)) => {
            fault(&mut stream, RequestId::unknown(), Fault::UnsupportedFrame).await;
            return;
        }
        Ok(Err(error)) => {
            fault(&mut stream, RequestId::unknown(), error).await;
            return;
        }
        Err(_) => {
            fault(&mut stream, RequestId::unknown(), Fault::ReadTimeout).await;
            return;
        }
    };
    if stream.has_pending_input() {
        fault(&mut stream, RequestId::unknown(), Fault::ExtraFrame).await;
        return;
    }
    let mut request: InboundRequest = match serde_json::from_slice(&frame.body) {
        Ok(request) => request,
        Err(_) => {
            fault(&mut stream, RequestId::unknown(), Fault::InvalidEnvelope).await;
            return;
        }
    };
    let validated = match validate(&mut request, &endpoint.registration.session_id) {
        Ok(validated) => validated,
        Err(error) => {
            fault(&mut stream, request.id, error).await;
            return;
        }
    };
    let authority = (|| {
        let process = peer::verify(frame.credentials).ok_or("GUI broker peer is unverifiable")?;
        let mut client = ClientIdentity::from_peer(process);
        client.attended_local = false;
        let peer = kernel_peer(&client)?;
        let bound = endpoint
            .process
            .get()
            .ok_or("GUI broker launch is not bound")?;
        if endpoint.stopped.load(Ordering::Acquire)
            || !bound
                .containment
                .is_peer(&peer)
                .map_err(|error| error.to_string())?
        {
            return Err("GUI broker peer is outside this live instance".to_string());
        }
        let view = endpoint.registration.snapshot(&bound.process)?;
        Ok((client, view))
    })();
    let (client, view) = match authority {
        Ok(authority) => authority,
        Err(error) => {
            respond(
                &mut stream,
                Response::handler_error(request.id, BrokerError::authorization(error)),
            )
            .await;
            return;
        }
    };
    let response = match validated {
        Validated::Policy => Response::ok(
            request.id,
            crate::worker::broker::policy_response(
                &endpoint.registration.session_id,
                Some(endpoint.registration.launch.app_id()),
                &view.caps,
                &request.params,
            ),
        ),
        Validated::Memory => {
            match crate::worker::broker::memory_call_with_caps(view.caps, &request.params) {
                Ok(value) => Response::ok(request.id, json!({ "result": value })),
                Err(error) => {
                    Response::handler_error(request.id, BrokerError::authorization(error))
                }
            }
        }
        Validated::Provider(request) => {
            crate::clawd::server::dispatch_verified_request(request, &client, &daemon, &admission)
                .await
        }
    };
    respond(&mut stream, response).await;
}

async fn fault(stream: &mut PeerStream, id: RequestId, fault: Fault) {
    tracing::warn!(class = fault.class(), "GUI broker request refused");
    respond(stream, Response::error(id, fault.code(), fault.message())).await;
}

async fn respond(stream: &mut PeerStream, response: Response) {
    let body = match encode_response(&response) {
        Ok(body) if body.len() <= MAX_RESPONSE_BYTES => body,
        Ok(_) => {
            tracing::error!("GUI broker response exceeded the wire bound");
            return;
        }
        Err(error) => {
            tracing::error!(%error, "GUI broker response encoding failed");
            return;
        }
    };
    match tokio::time::timeout(IO_TIMEOUT, stream.write_response(&body)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, "GUI broker response recipient closed"),
        Err(_) => tracing::warn!("GUI broker response deadline elapsed"),
    }
}

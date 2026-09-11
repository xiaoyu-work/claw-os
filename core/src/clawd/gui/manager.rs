use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::{Epoch, InstanceId};
use serde_json::{json, Value};

use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::BrokerError;
use crate::clawd::state::DaemonState;
use crate::clawd::transport::Admission;

pub(super) use super::inputs::Outcome;
use super::inputs::{InstanceRequest, LaunchRequest};

const MAX_JOBS: usize = 128;
const MAX_OWNER_JOBS: usize = 32;
static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Reserved,
    Starting,
    Running,
    Retiring,
    Finished,
}

pub(super) struct JobState {
    pub phase: Phase,
    pub outcome: Option<Outcome>,
    pub retirement_error: Option<String>,
    finished_at: Option<Instant>,
}

pub(super) struct Job {
    pub registration: Arc<Registration>,
    pub instance: InstanceId,
    pub stopped: AtomicBool,
    pub state: Mutex<JobState>,
}

impl Job {
    pub fn stopping(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub fn running(&self) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "GUI status lock poisoned")?;
        if self.stopping() {
            return Err("GUI launch was retired before execution".to_string());
        }
        state.phase = Phase::Running;
        Ok(())
    }

    pub fn retiring(&self, error: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = Phase::Retiring;
            state.retirement_error = error;
        } else {
            tracing::error!("GUI status lock poisoned during retirement");
        }
    }

    pub fn complete(&self, outcome: Outcome) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = Phase::Finished;
            state.outcome = Some(outcome);
            state.retirement_error = None;
            state.finished_at = Some(Instant::now());
        } else {
            tracing::error!("GUI status lock poisoned after checked retirement");
        }
    }

    fn request_stop(&self) -> Result<(), String> {
        self.stopped.store(true, Ordering::Release);
        let mut state = self.state.lock().map_err(|_| "GUI status lock poisoned")?;
        if state.phase == Phase::Reserved {
            crate::paths::RoutedPathContext::for_owner(
                self.registration.owner(),
                self.registration.home.clone(),
            )
            .scope_sync(|| self.registration.clear_after_retirement());
            state.phase = Phase::Finished;
            state.outcome = Some(Outcome::failed(
                "GUI registration retired before execution".to_string(),
            ));
            state.finished_at = Some(Instant::now());
        }
        Ok(())
    }
}

pub(crate) struct Manager {
    pub(super) daemon: DaemonState,
    pub(super) admission: Arc<Admission>,
    pub(super) executor: tokio::runtime::Handle,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl Manager {
    pub fn start(daemon: DaemonState, admission: Arc<Admission>) -> Result<Arc<Self>, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("GUI custody requires the Root broker".to_string());
        }
        let manager = Arc::new(Self {
            daemon,
            admission,
            executor: tokio::runtime::Handle::current(),
            jobs: Mutex::new(HashMap::new()),
        });
        MANAGER
            .set(manager.clone())
            .map_err(|_| "GUI custody is already initialized")?;
        let service = manager.clone();
        manager.executor.spawn(async move {
            loop {
                if let Err(error) = service.sweep() {
                    tracing::error!(%error, "GUI lifetime sweep failed");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
        Ok(manager)
    }

    pub fn get() -> Result<&'static Arc<Self>, String> {
        MANAGER
            .get()
            .ok_or_else(|| "Root GUI custody is unavailable".to_string())
    }

    pub fn reserve(&self, registration: Registration) -> Result<(), String> {
        let mut jobs = self.jobs.lock().map_err(|_| "GUI registry lock poisoned")?;
        if jobs.len() >= MAX_JOBS
            || jobs.contains_key(&registration.session_id)
            || jobs
                .values()
                .filter(|job| job.registration.owner() == registration.owner())
                .count()
                >= MAX_OWNER_JOBS
        {
            return Err("GUI registration is duplicate or at its lifetime bound".to_string());
        }
        let session = registration.session_id.clone();
        let instance = InstanceId::random().map_err(|error| error.to_string())?;
        jobs.insert(
            session,
            Arc::new(Job {
                registration: Arc::new(registration),
                instance,
                stopped: AtomicBool::new(false),
                state: Mutex::new(JobState {
                    phase: Phase::Reserved,
                    outcome: None,
                    retirement_error: None,
                    finished_at: None,
                }),
            }),
        );
        Ok(())
    }

    pub fn contains(session: &str) -> Result<bool, String> {
        match MANAGER.get() {
            Some(manager) => Ok(manager
                .jobs
                .lock()
                .map_err(|_| "GUI registry lock poisoned")?
                .contains_key(session)),
            None => Ok(false),
        }
    }

    fn lookup(&self, session: &str, client: &ClientIdentity) -> Result<Arc<Job>, BrokerError> {
        let job = self
            .jobs
            .lock()
            .map_err(|_| BrokerError::unavailable("GUI registry lock poisoned"))?
            .get(session)
            .cloned()
            .ok_or_else(|| BrokerError::authorization("GUI instance is unavailable"))?;
        job.registration
            .require_client(client)
            .map_err(BrokerError::authorization)?;
        Ok(job)
    }

    pub fn retire_where(
        &self,
        predicate: impl Fn(&Registration, InstanceId) -> bool,
        deadline: Instant,
    ) -> Result<(), String> {
        let jobs: Vec<_> = self
            .jobs
            .lock()
            .map_err(|_| "GUI registry lock poisoned")?
            .values()
            .filter(|job| predicate(&job.registration, job.instance))
            .cloned()
            .collect();
        for job in &jobs {
            job.request_stop()?;
        }
        for job in jobs {
            loop {
                let state = job.state.lock().map_err(|_| "GUI status lock poisoned")?;
                if state.phase == Phase::Finished {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(state
                        .retirement_error
                        .clone()
                        .unwrap_or_else(|| "GUI retirement barrier is still pending".to_string()));
                }
                drop(state);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }

    fn sweep(&self) -> Result<(), String> {
        let jobs: Vec<_> = self
            .jobs
            .lock()
            .map_err(|_| "GUI registry lock poisoned")?
            .values()
            .cloned()
            .collect();
        for job in jobs {
            let state = job.state.lock().map_err(|_| "GUI status lock poisoned")?;
            let expired = state.phase == Phase::Reserved
                && job.registration.registered_at.elapsed() > Duration::from_secs(120);
            let finished = state
                .finished_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(30));
            drop(state);
            let launcher_ended = job.registration.launcher.assert_current().is_err();
            if expired || launcher_ended {
                job.request_stop()?;
            }
            if finished && launcher_ended {
                self.jobs
                    .lock()
                    .map_err(|_| "GUI registry lock poisoned")?
                    .remove(&job.registration.session_id);
            }
        }
        Ok(())
    }
}

pub(crate) async fn launch(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: LaunchRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid GUI launch arguments"))?;
    let manager = Manager::get().map_err(BrokerError::unavailable)?;
    let job = manager.lookup(request.session_id.as_str(), client)?;
    job.registration
        .require_launch(request.handle.as_str(), client)
        .map_err(BrokerError::authorization)?;
    {
        let mut state = job
            .state
            .lock()
            .map_err(|_| BrokerError::unavailable("GUI status lock poisoned"))?;
        if state.phase != Phase::Reserved {
            return Err(BrokerError::authorization("GUI registration is one-use"));
        }
        state.phase = Phase::Starting;
    }
    let running = job.clone();
    let manager = manager.clone();
    let context = crate::paths::RoutedPathContext::for_owner(
        job.registration.owner(),
        job.registration.home.clone(),
    );
    if let Err(error) = std::thread::Builder::new()
        .name("gui-supervisor".to_string())
        .spawn(move || {
            context.scope_sync(|| super::supervision::run(running, manager, request));
        })
    {
        job.registration.clear_after_retirement();
        job.complete(Outcome::failed(format!("start GUI supervisor: {error}")));
        return Err(BrokerError::unavailable("GUI supervisor could not start"));
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let state = job
                .state
                .lock()
                .map_err(|_| BrokerError::unavailable("GUI status lock poisoned"))?;
            if state.phase == Phase::Running || state.phase == Phase::Finished {
                if let Some(outcome) = &state.outcome {
                    if let Some(error) = &outcome.error {
                        let error = if outcome.stderr.text.is_empty() {
                            error.clone()
                        } else {
                            format!("{error}\n{}", outcome.stderr.text)
                        };
                        return Err(BrokerError::execution(error));
                    }
                }
                return Ok(json!({ "started": true, "layer_shell": false }));
            }
            if Instant::now() >= deadline {
                job.stopped.store(true, Ordering::Release);
                return Err(BrokerError::indeterminate(
                    "GUI launch has not completed its startup/retirement barrier",
                ));
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(crate) async fn wait(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: InstanceRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid GUI instance reference"))?;
    let job = Manager::get()
        .map_err(BrokerError::unavailable)?
        .lookup(request.session_id.as_str(), client)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let state = job
                .state
                .lock()
                .map_err(|_| BrokerError::unavailable("GUI status lock poisoned"))?;
            if let Some(outcome) = &state.outcome {
                return Ok(json!({ "finished": true, "outcome": outcome }));
            }
            if Instant::now() >= deadline {
                return Ok(
                    json!({ "finished": false, "retiring": state.phase == Phase::Retiring, "retirement_error": state.retirement_error }),
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub(crate) async fn stop(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: InstanceRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid GUI instance reference"))?;
    let manager = Manager::get().map_err(BrokerError::unavailable)?.clone();
    let job = manager.lookup(request.session_id.as_str(), client)?;
    let session = job.registration.session_id.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    super::retirement::wait(deadline, move |deadline| {
        manager.retire_where(
            |registration, _| registration.session_id == session,
            deadline,
        )
    })
    .await
    .map_err(BrokerError::indeterminate)?;
    Ok(json!({ "retired": true }))
}

pub(crate) fn retire_session(owner: u32, session: &str, deadline: Instant) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        manager.retire_where(
            |registration, _| {
                registration.owner() == owner && registration.belongs_to_session(session)
            },
            deadline,
        )?;
    }
    Ok(())
}

pub(crate) fn retire_app(owner: u32, app: &str, deadline: Instant) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        manager.retire_where(
            |registration, _| registration.owner() == owner && registration.launch.app_id() == app,
            deadline,
        )?;
    }
    Ok(())
}

pub(crate) fn retire_owner(owner: u32, deadline: Instant) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        manager.retire_where(|registration, _| registration.owner() == owner, deadline)?;
    }
    Ok(())
}

pub(crate) fn retire_approval_scope(
    scope: &crate::approvals::RevocationScope,
    deadline: Instant,
) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        manager.retire_where(
            |registration, _| {
                super::retirement::approval_matches(scope, registration.owner(), |session| {
                    registration.belongs_to_session(session)
                })
            },
            deadline,
        )?;
    }
    Ok(())
}

pub(crate) fn retire_epoch(epoch: Epoch, deadline: Instant) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        manager.retire_where(
            |registration, _| registration.control.epoch == epoch,
            deadline,
        )?;
    }
    Ok(())
}

pub(crate) fn compositor_retired(epoch: Epoch, instance: InstanceId) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        let jobs = manager
            .jobs
            .lock()
            .map_err(|_| "GUI registry lock poisoned")?;
        for job in jobs.values() {
            if job.registration.control.epoch == epoch && job.instance == instance {
                job.stopped.store(true, Ordering::Release);
            }
        }
    }
    Ok(())
}

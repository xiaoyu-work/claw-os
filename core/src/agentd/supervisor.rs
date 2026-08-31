//! Broker-side supervision of `claw-agentd` workers.
//!
//! `clawd` remains the authority for the task queue: it claims a job,
//! derives the session's capabilities from root-owned metadata, spawns
//! exactly one unprivileged worker for it, leases that worker a grant
//! bound to the pid the kernel just allocated, and persists everything
//! the worker reports. The worker never opens the queue, the audit log
//! or the broker socket.
//!
//! Failure is contained here as well. A worker that panics, is killed,
//! exits without a result, stops heartbeating, sends a frame outside
//! its grant, or speaks a different protocol version only ever ends its
//! own task: the supervisor reaps it, marks the task for retry or
//! failure, and keeps serving. `clawd` treats no worker exit — normal
//! or not — as fatal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;

use crate::agent::service::{FinishOutcome, Job, Store};
use crate::caps::ConsentContext;
use crate::clawd::state::DaemonState;
use crate::clawd::transport::limits::{Admission, Limits};

use super::grant::{GrantClaims, GrantExpectation, GrantSigner, GRANT_AUDIENCE, GRANT_VERSION};
use super::protocol::{
    self, ApprovalAsk, ApprovalReply, Assignment, BrokerFrame, FrameReader, JobSpec,
    RuntimeAuditRecord, WorkerFrame, WorkerHello, WorkerOutcome,
};
use super::spawn;

const DEFAULT_MAX_WORKERS: usize = 4;
const DEFAULT_POLL_MS: u64 = 500;
/// How long a worker may hold a task without a heartbeat before the
/// supervisor reclaims it.
const DEFAULT_LEASE_SECS: u64 = 900;
const DEFAULT_HEARTBEAT_GRACE_SECS: u64 = 120;
const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const PUMP_TICK: Duration = Duration::from_millis(250);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long a cancelled worker gets to unwind its provider call and
/// report a cancellation before the supervisor kills it.
const CANCEL_GRACE: Duration = Duration::from_secs(15);
/// Ceiling on how fast failed spawns may be retried, so a broken worker
/// image cannot turn the queue into a fork bomb.
const SPAWN_BACKOFF_MAX: Duration = Duration::from_secs(60);
const SPAWN_BACKOFF_BASE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub enabled: bool,
    pub poll: Duration,
    pub max_workers: usize,
    pub lease: Duration,
    pub heartbeat_grace: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll: Duration::from_millis(DEFAULT_POLL_MS),
            max_workers: DEFAULT_MAX_WORKERS,
            lease: Duration::from_secs(DEFAULT_LEASE_SECS),
            heartbeat_grace: Duration::from_secs(DEFAULT_HEARTBEAT_GRACE_SECS),
        }
    }
}

impl SupervisorConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("CLAWD_AGENTD") {
            config.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no" | "disabled"
            );
        }
        if let Some(value) = env_u64("CLAWD_AGENTD_POLL_MS") {
            config.poll = Duration::from_millis(value.clamp(50, 60_000));
        }
        if let Some(value) = env_u64("CLAWD_AGENTD_MAX_WORKERS") {
            config.max_workers = value.clamp(1, 64) as usize;
        }
        if let Some(value) = env_u64("CLAWD_AGENTD_LEASE_SECS") {
            config.lease = Duration::from_secs(value.clamp(30, 86_400));
        }
        if let Some(value) = env_u64("CLAWD_AGENTD_HEARTBEAT_GRACE_SECS") {
            config.heartbeat_grace = Duration::from_secs(value.clamp(10, 3_600));
        }
        config
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

/// Start supervision on the daemon's runtime. The returned handle is
/// deliberately not joined into `clawd`'s fatal path: supervision
/// stopping must never take the broker down with it.
#[derive(Clone)]
pub struct BrokerContext {
    pub state: DaemonState,
    pub admission: Arc<Admission>,
    pub primary_socket: std::path::PathBuf,
    pub isolated_execution_gid: u32,
    extension_containment:
        Arc<OnceLock<Result<Arc<crate::extension_host::spawn::ContainmentRoot>, Arc<str>>>>,
    extension_identities: Arc<
        OnceLock<Result<Arc<crate::extension_host::identity::ExtensionIdentityPool>, Arc<str>>>,
    >,
}

impl BrokerContext {
    pub fn new(
        state: DaemonState,
        admission: Arc<Admission>,
        primary_socket: std::path::PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            state,
            admission,
            primary_socket,
            isolated_execution_gid: spawn::resolve_isolated_execution_gid()?,
            extension_containment: Arc::new(OnceLock::new()),
            extension_identities: Arc::new(OnceLock::new()),
        })
    }

    fn extension_containment(
        &self,
    ) -> Result<Arc<crate::extension_host::spawn::ContainmentRoot>, String> {
        match self.extension_containment.get_or_init(|| {
            crate::extension_host::spawn::ContainmentRoot::establish()
                .map(Arc::new)
                .map_err(Arc::<str>::from)
        }) {
            Ok(root) => Ok(root.clone()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn prime_extension_containment(&self) {
        if let Err(error) = self.extension_containment() {
            tracing::error!(
                error = %error,
                "extension containment unavailable; agent tasks will fail closed"
            );
        }
    }

    fn extension_identity_pool(
        &self,
    ) -> Result<Arc<crate::extension_host::identity::ExtensionIdentityPool>, String> {
        match self.extension_identities.get_or_init(|| {
            crate::extension_host::identity::ExtensionIdentityPool::load(
                self.isolated_execution_gid,
            )
            .map_err(Arc::<str>::from)
        }) {
            Ok(pool) => Ok(pool.clone()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn prime_extension_identities(&self) {
        if let Err(error) = self.extension_identity_pool() {
            tracing::error!(
                error = %error,
                "extension execution uid pool unavailable; agent tasks will fail closed"
            );
        }
    }
}

pub fn spawn_supervisor(
    shutdown: Arc<AtomicBool>,
    broker: BrokerContext,
) -> tokio::task::JoinHandle<()> {
    let config = SupervisorConfig::from_env();
    if config.enabled {
        broker.prime_extension_containment();
        broker.prime_extension_identities();
    }
    tokio::spawn(async move {
        if !config.enabled {
            tracing::warn!(
                "agentd supervision disabled by CLAWD_AGENTD; agent tasks will not run, \
                 other clawd primitives are unaffected"
            );
            return;
        }
        if let Err(error) = run_with_broker(config, shutdown, broker).await {
            tracing::error!(
                error = %error,
                "agentd supervision stopped; clawd continues serving non-agent primitives"
            );
        }
    })
}

pub async fn run(config: SupervisorConfig, shutdown: Arc<AtomicBool>) -> Result<(), String> {
    let broker = BrokerContext::new(
        DaemonState::try_new()?,
        Admission::new(Limits::default()),
        crate::paths::clawd_socket_path(),
    )?;
    run_with_broker(config, shutdown, broker).await
}

async fn run_with_broker(
    config: SupervisorConfig,
    shutdown: Arc<AtomicBool>,
    broker: BrokerContext,
) -> Result<(), String> {
    let store = Store::open_default().map_err(|error| error.to_string())?;
    run_with_store_and_broker(config, shutdown, store, broker).await
}

/// Supervision loop against an explicit queue. `run` uses the daemon's
/// store; tests drive the same code path against a temporary one, so
/// the claim → spawn → lease → pump → finish sequence they exercise is
/// the production sequence.
pub async fn run_with_store(
    config: SupervisorConfig,
    shutdown: Arc<AtomicBool>,
    store: Store,
) -> Result<(), String> {
    let broker = BrokerContext::new(
        DaemonState::try_new()?,
        Admission::new(Limits::default()),
        crate::paths::clawd_socket_path(),
    )?;
    run_with_store_and_broker(config, shutdown, store, broker).await
}

pub async fn run_with_store_and_broker(
    config: SupervisorConfig,
    shutdown: Arc<AtomicBool>,
    store: Store,
    broker: BrokerContext,
) -> Result<(), String> {
    if config.enabled {
        broker.prime_extension_containment();
        broker.prime_extension_identities();
    }
    let signer = Arc::new(GrantSigner::generate()?);
    let permits = Arc::new(Semaphore::new(config.max_workers));
    let throttle = Arc::new(Mutex::new(SpawnThrottle::default()));
    let broker_pid = std::process::id();

    // A worker cannot outlive the daemon that leased it — `PDEATHSIG`
    // kills it — so on start-up every task still sitting in `running/`
    // belongs to a dead worker. Reconciling before the first claim is
    // what makes a restart or upgrade self-healing.
    reconcile(&store);
    let mut last_reconcile = Instant::now();

    tracing::info!(
        max_workers = config.max_workers,
        lease_secs = config.lease.as_secs(),
        worker = %spawn::worker_binary_path().display(),
        "agentd supervision started"
    );

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if last_reconcile.elapsed() >= RECONCILE_INTERVAL {
            reconcile(&store);
            last_reconcile = Instant::now();
        }
        if let Some(wait) = throttle.lock().map(|t| t.wait()).unwrap_or(None) {
            sleep_interruptible(wait, &shutdown).await;
            continue;
        }
        let Ok(permit) = permits.clone().acquire_owned().await else {
            break;
        };
        let claimed = match crate::clawd::tasks::claim_job_with_presence(&store, config.lease) {
            Ok(claimed) => claimed,
            Err(error) => {
                tracing::warn!(error = %error, "agentd supervisor failed to claim a task");
                drop(permit);
                sleep_interruptible(config.poll, &shutdown).await;
                continue;
            }
        };
        let Some((job, presence)) = claimed else {
            drop(permit);
            sleep_interruptible(config.poll, &shutdown).await;
            continue;
        };

        let store = store.clone();
        let signer = signer.clone();
        let config = config.clone();
        let throttle = throttle.clone();
        let shutdown = shutdown.clone();
        let broker = broker.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let job_id = job.id.clone();
            // A panic in the supervision path must not escape into the
            // daemon's runtime, and must still resolve the task.
            let supervised = supervise(
                store.clone(),
                signer,
                config,
                throttle,
                shutdown,
                broker_pid,
                broker,
                job,
                presence,
            )
            .await;
            if let Err(error) = supervised {
                tracing::warn!(task = %job_id, error = %error, "agentd task supervision failed");
            }
        });
    }
    Ok(())
}

async fn sleep_interruptible(total: Duration, shutdown: &Arc<AtomicBool>) {
    let slice = Duration::from_millis(100).min(total);
    let start = Instant::now();
    while start.elapsed() < total {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(slice).await;
    }
}

fn reconcile(store: &Store) {
    match store.recover_orphaned_jobs() {
        Ok((requeued, failed)) if requeued > 0 || failed > 0 => {
            tracing::info!(requeued, failed, "agentd reconciled stale worker leases");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(error = %error, "agentd lease reconciliation failed"),
    }
}

#[derive(Debug, Default)]
struct SpawnThrottle {
    failures: u32,
    next_allowed: Option<Instant>,
}

impl SpawnThrottle {
    fn wait(&self) -> Option<Duration> {
        let next = self.next_allowed?;
        let now = Instant::now();
        (next > now).then(|| next - now)
    }

    fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let backoff = SPAWN_BACKOFF_BASE
            .saturating_mul(1u32 << self.failures.min(6))
            .min(SPAWN_BACKOFF_MAX);
        self.next_allowed = Some(Instant::now() + backoff);
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.next_allowed = None;
    }
}

/// Everything the broker knows about one leased worker. Used to check
/// every frame against the process the kernel actually gave us.
struct Lease {
    task_id: String,
    session_id: Option<String>,
    owner_uid: u32,
    execution_gid: u32,
    client: crate::session::SessionClient,
    presence: Option<crate::session::SessionPresence>,
    capability_generation: String,
    extension: Option<crate::extension_host::protocol::ExtensionBinding>,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    deadline: Instant,
    approval_expires_at: u64,
    approval_nonce: String,
    consent_context: ConsentContext,
}

impl Lease {
    fn approval_identity(&self) -> crate::approvals::ApprovalExecutionIdentity {
        crate::approvals::ApprovalExecutionIdentity {
            task_id: self.task_id.clone(),
            worker_pid: self.worker_pid,
            worker_start_time_ticks: self.worker_start_time_ticks,
            lease_nonce: self.approval_nonce.clone(),
        }
    }
}

async fn supervise(
    store: Store,
    signer: Arc<GrantSigner>,
    config: SupervisorConfig,
    throttle: Arc<Mutex<SpawnThrottle>>,
    shutdown: Arc<AtomicBool>,
    broker_pid: u32,
    broker: BrokerContext,
    job: Job,
    presence: Option<crate::session::SessionPresence>,
) -> Result<(), String> {
    let Some(owner_uid) = job.owner_uid else {
        finish_error(
            &store,
            job,
            "task has no recorded owner; refusing to run it in a privileged context",
        );
        return Ok(());
    };
    // Refused here, before a worker is forked or any provider, MCP
    // client or tool registry is constructed: a root-owned task has no
    // account to drop to, so running it would put the model back in a
    // root process.
    if owner_uid == 0 {
        finish_error(&store, job, spawn::ROOT_OWNER_REFUSAL);
        return Ok(());
    }
    let identity = match spawn::resolve_identity(owner_uid) {
        Ok(identity) => identity,
        Err(error) => {
            finish_error(&store, job, &error);
            return Ok(());
        }
    };
    let isolation = match spawn::ExecutionIsolation::capture(
        &broker.primary_socket,
        owner_uid,
        broker.isolated_execution_gid,
    ) {
        Ok(isolation) => isolation,
        Err(error) => {
            finish_error(
                &store,
                job,
                &format!("agent isolation boundary unavailable: {error}"),
            );
            return Ok(());
        }
    };
    let containment = match broker.extension_containment() {
        Ok(containment) => containment,
        Err(error) => {
            finish_error(
                &store,
                job,
                &format!("extension containment boundary unavailable: {error}"),
            );
            return Ok(());
        }
    };
    if let Err(error) = crate::storage::ensure_owner_agent_state_dir(owner_uid, identity.gid) {
        finish_error(
            &store,
            job,
            &format!("failed to prepare owner agent state: {error}"),
        );
        return Ok(());
    }

    // Capabilities are derived here, from root-owned session metadata,
    // and handed to the worker. Nothing the worker says can widen them.
    let (mut session, consent_context) = match job.session_id.as_deref() {
        Some(session_id) => match broker_session_info(session_id) {
            Ok(session) => session,
            Err(error) => {
                finish_error(&store, job, &format!("session unavailable: {error}"));
                return Ok(());
            }
        },
        None => (None, ConsentContext::Unattended),
    };
    let mut effective_client = if job.client.source != crate::session::SessionSource::Unknown {
        job.client
    } else {
        session
            .as_ref()
            .map(|session| session.client)
            .unwrap_or_default()
    };
    // `attended` is never durable authority. A live, in-memory submission
    // presence lease is the only way a queued task may recover attendance.
    effective_client.attended = false;
    if let Some(session) = session.as_mut() {
        session.client = effective_client;
    }

    let spawned = match spawn::spawn_worker(&identity, &isolation, &job.id) {
        Ok(spawned) => {
            if let Ok(mut throttle) = throttle.lock() {
                throttle.record_success();
            }
            spawned
        }
        Err(error) => {
            if let Ok(mut throttle) = throttle.lock() {
                throttle.record_failure();
            }
            release_or_fail(
                &store,
                &job,
                &format!("failed to start agent worker: {error}"),
            );
            return Ok(());
        }
    };

    let spawn::SpawnedWorker {
        mut child,
        channel,
        pid,
        start_time_ticks,
    } = spawned;

    drain_worker_output(&mut child, &job.id);

    // Rebind the queue record from the claiming broker to the process
    // that actually runs the work, so recovery and audit both point at
    // the worker rather than at `clawd`.
    if let Err(error) = store.bind_worker(&job.id, pid, start_time_ticks) {
        tracing::warn!(task = %job.id, error = %error, "failed to bind agent worker to task");
    }

    let mut extension = match start_extension_host(
        &identity,
        &isolation,
        &containment,
        &job,
        session.as_ref(),
        pid,
        start_time_ticks,
        config.lease,
        broker,
    )
    .await
    {
        Ok(extension) => extension,
        Err(error) => {
            reap(&mut child, pid).await;
            release_or_fail(
                &store,
                &job,
                &format!("failed to start extension host: {error}"),
            );
            return Ok(());
        }
    };
    crate::clawd::audit::record_extension_host_event(
        &job.id,
        job.session_id.as_deref(),
        owner_uid,
        extension.host.pid,
        extension.host.start_time_ticks,
        "attach",
        true,
    );

    let lease = Lease {
        task_id: job.id.clone(),
        session_id: job.session_id.clone(),
        owner_uid,
        execution_gid: isolation.execution_gid(),
        client: effective_client,
        presence,
        capability_generation: session
            .as_ref()
            .and_then(|session| session.caps.as_ref())
            .map(crate::agent::tools::exposure::capability_generation)
            .unwrap_or_else(|| {
                crate::agent::tools::exposure::capability_generation(&crate::caps::CapSet::new())
            }),
        extension: Some(extension.host.binding.clone()),
        worker_pid: pid,
        worker_start_time_ticks: start_time_ticks,
        deadline: Instant::now() + config.lease,
        approval_expires_at: approval_deadline(config.lease),
        approval_nonce: uuid::Uuid::new_v4().to_string(),
        consent_context,
    };
    let approval_identity = lease.approval_identity();

    let outcome = pump(
        &store,
        &signer,
        &config,
        &shutdown,
        broker_pid,
        &job,
        session,
        lease,
        channel,
        &mut child,
        &mut extension.host.child,
        &extension.host.cgroup,
        extension.lease.clone(),
    )
    .await;

    extension.lease.close();
    extension.broker_task.abort();
    reap(&mut child, pid).await;
    if let Some(session_id) = extension.host_session_id.as_deref() {
        for child_session in crate::proc::deregister_child_sessions_for_owner(session_id, owner_uid)
        {
            crate::clawd::authority::revoke_session_for_owner(&child_session, owner_uid);
        }
        crate::proc::deregister_session_for_owner(session_id, owner_uid);
        crate::clawd::authority::revoke_session_for_owner(session_id, owner_uid);
    }
    let containment_cleanup = reap_extension_host(&mut extension.host).await;
    let extension_cleanup = match containment_cleanup {
        Ok(()) => {
            crate::storage::remove_routed_extension_reader(owner_uid, extension.extension_uid)
        }
        Err(error) => Err(error),
    };
    if extension_cleanup.is_ok() {
        if let Some(identity) = extension.identity.take() {
            identity.release();
        }
    }
    crate::clawd::audit::record_extension_host_event(
        &job.id,
        job.session_id.as_deref(),
        owner_uid,
        extension.host.pid,
        extension.host.start_time_ticks,
        if extension_cleanup.is_ok() {
            "detach"
        } else {
            "cleanup-failed"
        },
        extension_cleanup.is_ok(),
    );
    extension.host.paths.cleanup();

    // The worker's lease is over, so every grant its session accrued
    // goes with it — including any reusable approval the user made
    // "for this session". A grant outliving the process it was bound to
    // is exactly what the authority exists to prevent.
    if let Some(session_id) = job.session_id.as_deref() {
        if let Err(error) = crate::approvals::invalidate_pending_for_execution(
            Some(owner_uid),
            session_id,
            &approval_identity,
            "agent task or worker lease ended before approval",
        ) {
            tracing::error!(
                task = %job.id,
                error = %error,
                "could not invalidate pending approvals for a finished worker"
            );
        }
        crate::clawd::authority::revoke_session_for_owner(session_id, owner_uid);
    }

    let outcome = apply_containment_cleanup(outcome, extension_cleanup);
    finish_task_outcome(&store, &job, outcome);
    Ok(())
}

fn finish_task_outcome(store: &Store, job: &Job, outcome: TaskOutcome) {
    match outcome {
        TaskOutcome::Reported(outcome) => {
            let finish: FinishOutcome = (*outcome).into();
            if let Err(error) = store.finish(job.clone(), finish) {
                tracing::warn!(task = %job.id, error = %error, "failed to persist agent task result");
            }
        }
        TaskOutcome::Cancelled => {
            if let Err(error) = store.finish(job.clone(), FinishOutcome::Cancelled) {
                tracing::warn!(task = %job.id, error = %error, "failed to persist agent task cancellation");
            }
        }
        TaskOutcome::Failed(message) => {
            if let Err(error) = store.finish(job.clone(), FinishOutcome::Error(message)) {
                tracing::warn!(task = %job.id, error = %error, "failed to persist agent task failure");
            }
        }
        TaskOutcome::Retry(reason) => release_or_fail(store, job, &reason),
    }
}

enum TaskOutcome {
    Reported(Box<WorkerOutcome>),
    Cancelled,
    Failed(String),
    Retry(String),
}

fn apply_containment_cleanup(outcome: TaskOutcome, cleanup: Result<(), String>) -> TaskOutcome {
    match cleanup {
        Ok(()) => outcome,
        Err(error) => TaskOutcome::Failed(format!(
            "extension containment cleanup failed; descendants may remain: {error}"
        )),
    }
}

fn post_assignment_interruption(detail: String, cancel_sent: bool) -> TaskOutcome {
    if cancel_sent {
        TaskOutcome::Cancelled
    } else {
        // Once a complete assignment reached the worker, the protocol permits
        // execution to begin. No later crash/EOF/timeout can prove that an
        // external side effect did not happen, so replaying the task is unsafe.
        TaskOutcome::Failed(detail)
    }
}

struct ExtensionRuntime {
    host: crate::extension_host::spawn::SpawnedExtensionHost,
    identity: Option<crate::extension_host::identity::ExtensionIdentityLease>,
    extension_uid: u32,
    lease: Arc<crate::extension_host::broker::ExtensionLease>,
    broker_task: tokio::task::JoinHandle<()>,
    host_session_id: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn start_extension_host(
    identity: &spawn::WorkerIdentity,
    isolation: &spawn::ExecutionIsolation,
    containment: &crate::extension_host::spawn::ContainmentRoot,
    job: &Job,
    session: Option<&crate::proc::SessionInfo>,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    lease_duration: Duration,
    broker: BrokerContext,
) -> Result<ExtensionRuntime, String> {
    let mut execution_identity = broker.extension_identity_pool()?.acquire(identity.uid)?;
    let extension = execution_identity.identity().clone();
    let paths = crate::extension_host::spawn::HostPaths::create(identity)?;
    let listener = match crate::extension_host::broker::bind_listener(
        &paths,
        extension.uid,
        isolation.execution_gid(),
    ) {
        Ok(listener) => listener,
        Err(error) => {
            paths.cleanup();
            return Err(error);
        }
    };
    if let Err(error) = crate::storage::install_routed_extension_reader(identity.uid, extension.uid)
    {
        drop(listener);
        paths.cleanup();
        return Err(error);
    }
    let host_session_id = session.map(|_| format!("extension-{}", uuid::Uuid::new_v4().simple()));
    let lease_nonce = uuid::Uuid::new_v4().simple().to_string();
    let expires_at_ms = super::grant::now_ms().saturating_add(lease_duration.as_millis() as u64);
    let cleanup_paths = paths.clone();
    execution_identity.retain_until_cleanup();
    let mut host = match crate::extension_host::spawn::spawn_host(
        identity,
        &extension,
        isolation,
        containment,
        &job.id,
        job.session_id.as_deref(),
        host_session_id.as_deref(),
        worker_pid,
        worker_start_time_ticks,
        &lease_nonce,
        expires_at_ms,
        paths,
    ) {
        Ok(host) => host,
        Err(error) => {
            cleanup_paths.cleanup();
            return Err(error);
        }
    };
    drain_extension_output(&mut host.child, &job.id);

    if let (Some(host_session_id), Some(parent)) = (host_session_id.as_ref(), session) {
        let info = crate::proc::SessionInfo {
            session_id: host_session_id.clone(),
            pid: host.pid,
            command: vec!["claw-extension-host".to_string(), job.id.clone()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some(crate::extension_host::protocol::EXTENSION_HOST_GROUP.to_string()),
            parent: job.session_id.clone(),
            workdir: Some(host.paths.control_dir.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: parent.tier,
            scope: parent.scope.clone(),
            priority: parent.priority.clone(),
            caps: parent.caps.clone(),
            transient_caps: None,
            role: parent.role.clone(),
            app_id: None,
            pending_bind: false,
            start_time_ticks: host.start_time_ticks,
            client: crate::session::SessionClient::new(
                crate::session::SessionSource::BrokerTask,
                false,
                true,
            ),
        };
        if let Err(error) = crate::proc::register_session_for_owner(info, identity.uid) {
            let cleanup = host.cgroup.cleanup().await;
            let _ = host.child.start_kill();
            let _ = host.child.wait().await;
            host.paths.cleanup();
            let reader_cleanup = if cleanup.is_ok() {
                crate::storage::remove_routed_extension_reader(identity.uid, extension.uid)
            } else {
                Err("containment cleanup was not verified".to_string())
            };
            if cleanup.is_ok() && reader_cleanup.is_ok() {
                execution_identity.release();
            }
            let mut message = format!("register extension-host session: {error}");
            if let Err(cleanup) = cleanup {
                message.push_str(&format!("; containment cleanup failed: {cleanup}"));
            }
            if let Err(reader_cleanup) = reader_cleanup {
                message.push_str(&format!("; routed ACL cleanup failed: {reader_cleanup}"));
            }
            return Err(message);
        }
    }

    let lease = Arc::new(crate::extension_host::broker::ExtensionLease::new(
        job.id.clone(),
        job.session_id.clone(),
        host_session_id.clone(),
        identity.uid,
        extension.uid,
        isolation.execution_gid(),
        worker_pid,
        worker_start_time_ticks,
        host.pid,
        host.start_time_ticks,
        expires_at_ms,
    ));
    let broker_task = tokio::spawn(crate::extension_host::broker::serve(
        listener,
        lease.clone(),
        broker.state,
        broker.admission,
    ));
    Ok(ExtensionRuntime {
        host,
        identity: Some(execution_identity),
        extension_uid: extension.uid,
        lease,
        broker_task,
        host_session_id,
    })
}

#[allow(clippy::too_many_arguments)]
async fn pump(
    store: &Store,
    signer: &GrantSigner,
    config: &SupervisorConfig,
    shutdown: &Arc<AtomicBool>,
    broker_pid: u32,
    job: &Job,
    session: Option<crate::proc::SessionInfo>,
    mut lease: Lease,
    channel: tokio::net::UnixStream,
    child: &mut tokio::process::Child,
    extension_child: &mut tokio::process::Child,
    extension_cgroup: &crate::extension_host::spawn::ResourceGroup,
    extension_lease: Arc<crate::extension_host::broker::ExtensionLease>,
) -> TaskOutcome {
    // Authority on this channel comes from the grant, not from the
    // socket: `socketpair` is created before the fork, so `SO_PEERCRED`
    // is stamped with *clawd's* own uid and pid and says nothing about
    // the worker. The descriptor is private and never sent anywhere,
    // and every frame is checked against a grant bound to the pid and
    // start-time the kernel gave this child.
    let (reader, mut writer) = channel.into_split();
    let mut frames = FrameReader::new(BufReader::new(reader));

    let assignment = Assignment {
        protocol: protocol::PROTOCOL_VERSION,
        grant: signer.issue(claims_for(broker_pid, &lease, config.lease)),
        job: JobSpec {
            id: job.id.clone(),
            prompt: job.prompt.clone(),
            context: job.context.clone(),
            branch_context: job.branch_context.clone(),
            session_id: job.session_id.clone(),
            max_turns: job.max_turns,
            owner_uid: lease.owner_uid,
            owner_home: job.owner_home.clone().unwrap_or_default(),
        },
        consent_context: lease.consent_context,
        session,
        presence: lease.presence,
        extension: lease.extension.clone(),
    };
    if let Err(error) = send_assignment(&mut writer, assignment).await {
        return TaskOutcome::Retry(format!("failed to assign task to worker: {error}"));
    }

    let mut hello_seen = false;
    let mut cancel_sent = false;
    let mut cancelled_at: Option<Instant> = None;
    let mut approvals_used: u32 = 0;
    let mut last_progress = Instant::now();
    let mut ticker = tokio::time::interval(PUMP_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            frame = frames.next_frame::<WorkerFrame>() => match frame {
                Ok(Some(frame)) => {
                    last_progress = Instant::now();
                    match accept(signer, broker_pid, &mut lease, &frame, hello_seen) {
                        Ok(()) => {}
                        Err(error) => {
                            return TaskOutcome::Failed(format!(
                                "agent worker frame rejected: {error}"
                            ));
                        }
                    }
                    match frame {
                        WorkerFrame::Hello(hello) => {
                            if let Err(error) = check_hello(&hello, &lease) {
                                return TaskOutcome::Failed(error);
                            }
                            hello_seen = true;
                        }
                        WorkerFrame::Stream { event, .. } => {
                            if let Err(error) = store.append_stream_event(&job.id, &event) {
                                tracing::warn!(task = %job.id, error = %error, "failed to append agent stream event");
                            }
                        }
                        WorkerFrame::Progress { progress, .. } => {
                            if let Err(error) =
                                store.append_stream_progress(&job.id, progress.to_stream_value())
                            {
                                tracing::warn!(task = %job.id, error = %error, "failed to append agent progress");
                            }
                        }
                        WorkerFrame::Audit { record, .. } => {
                            record_worker_audit(&lease, &record);
                        }
                        WorkerFrame::Approval {
                            correlation_id,
                            exchange,
                            ..
                        } => {
                            let reply =
                                mediate_approval(&mut approvals_used, &lease, &exchange.ask);
                            let _ = send(
                                &mut writer,
                                &BrokerFrame::ApprovalReply {
                                    correlation_id,
                                    exchange,
                                    reply,
                                },
                            )
                            .await;
                        }
                        WorkerFrame::Heartbeat { .. } => {
                            lease.deadline = Instant::now() + config.lease;
                            lease.approval_expires_at = approval_deadline(config.lease);
                            extension_lease.renew(config.lease);
                        }
                        WorkerFrame::Result { outcome, .. } => {
                            return TaskOutcome::Reported(outcome);
                        }
                    }
                }
                Ok(None) => {
                    return post_assignment_interruption(
                        "agent worker closed its channel without reporting a result".to_string(),
                        cancel_sent,
                    );
                }
                Err(error) => {
                    return TaskOutcome::Failed(format!("agent worker protocol fault: {error}"));
                }
            },
            status = child.wait() => {
                let detail = match status {
                    Ok(status) => format!("agent worker exited early ({status})"),
                    Err(error) => format!("agent worker could not be reaped: {error}"),
                };
                return post_assignment_interruption(detail, cancel_sent);
            },
            status = extension_child.wait() => {
                let detail = match status {
                    Ok(status) => format!("extension host exited early ({status})"),
                    Err(error) => format!("extension host could not be reaped: {error}"),
                };
                crate::clawd::audit::record_extension_host_event(
                    &lease.task_id,
                    lease.session_id.as_deref(),
                    lease.owner_uid,
                    lease.extension.as_ref().map(|binding| binding.host_pid).unwrap_or_default(),
                    lease.extension.as_ref().and_then(|binding| binding.host_start_time_ticks),
                    "crash",
                    false,
                );
                return post_assignment_interruption(detail, cancel_sent);
            },
            _ = ticker.tick() => {
                if !cancel_sent
                    && (shutdown.load(Ordering::SeqCst)
                        || store.cancellation_requested(&job.id).unwrap_or(false))
                {
                    cancel_sent = true;
                    cancelled_at = Some(Instant::now());
                    crate::clawd::audit::record_extension_host_event(
                        &lease.task_id,
                        lease.session_id.as_deref(),
                        lease.owner_uid,
                        lease.extension.as_ref().map(|binding| binding.host_pid).unwrap_or_default(),
                        lease.extension.as_ref().and_then(|binding| binding.host_start_time_ticks),
                        "cancel",
                        true,
                    );
                    let _ = send(
                        &mut writer,
                        &BrokerFrame::Cancel { task_id: job.id.clone() },
                    )
                    .await;
                }
                // The worker still uses its unreaped process-group leader.
                // Dynamic descendants are terminated through the mandatory
                // cgroup, which remains authoritative even if the host exits
                // or a child daemonizes before cancellation.
                if cancelled_at.is_some_and(|at| at.elapsed() > CANCEL_GRACE) {
                    let _ = child.start_kill();
                    unsafe {
                        spawn::terminate_worker_group(lease.worker_pid, libc::SIGKILL);
                    }
                    if let Err(error) = extension_cgroup.kill_all() {
                        return TaskOutcome::Failed(format!(
                            "failed to terminate extension containment after cancellation: {error}"
                        ));
                    }
                }
                if !hello_seen && last_progress.elapsed() > config.heartbeat_grace {
                    return TaskOutcome::Failed(
                        "agent worker never completed the protocol handshake".to_string(),
                    );
                }
                if Instant::now() > lease.deadline {
                    let _ = child.start_kill();
                    unsafe {
                        spawn::terminate_worker_group(lease.worker_pid, libc::SIGKILL);
                        crate::clawd::audit::record_extension_host_event(
                            &lease.task_id,
                            lease.session_id.as_deref(),
                            lease.owner_uid,
                            lease.extension.as_ref().map(|binding| binding.host_pid).unwrap_or_default(),
                            lease.extension.as_ref().and_then(|binding| binding.host_start_time_ticks),
                            "timeout",
                            false,
                        );
                    }
                    if let Err(error) = extension_cgroup.kill_all() {
                        return TaskOutcome::Failed(format!(
                            "failed to terminate timed-out extension containment: {error}"
                        ));
                    }
                    return TaskOutcome::Failed(
                        "agent worker lease expired without a heartbeat".to_string(),
                    );
                }
            },
        }
    }
}

fn claims_for(broker_pid: u32, lease: &Lease, ttl: Duration) -> GrantClaims {
    let issued_at_ms = super::grant::now_ms();
    GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid,
        task_id: lease.task_id.clone(),
        session_id: lease.session_id.clone(),
        owner_uid: lease.owner_uid,
        owner_gid: lease.execution_gid,
        client: lease.client,
        presence: lease.presence,
        capability_generation: lease.capability_generation.clone(),
        extension: lease.extension.clone(),
        worker_pid: lease.worker_pid,
        worker_start_time_ticks: lease.worker_start_time_ticks,
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(ttl.as_millis() as u64),
        routes: protocol::worker_routes(),
    }
}

fn approval_deadline(ttl: Duration) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(ttl.as_secs())
}

/// Route and binding check applied to every frame, not just the
/// handshake: task, owner, worker identity and lease are all re-checked
/// before the frame is allowed to touch broker state.
fn accept(
    signer: &GrantSigner,
    broker_pid: u32,
    lease: &mut Lease,
    frame: &WorkerFrame,
    hello_seen: bool,
) -> Result<(), String> {
    let route = frame.route();
    if !protocol::WORKER_ROUTES.contains(&route) {
        return Err(format!("route `{route}` is not on the worker channel"));
    }
    match frame {
        WorkerFrame::Hello(hello) => {
            if hello_seen {
                return Err("duplicate handshake".to_string());
            }
            let expect = GrantExpectation {
                broker_pid,
                task_id: lease.task_id.clone(),
                session_id: lease.session_id.clone(),
                owner_uid: lease.owner_uid,
                owner_gid: lease.execution_gid,
                client: lease.client,
                presence: lease.presence,
                capability_generation: lease.capability_generation.clone(),
                extension: lease.extension.clone(),
                worker_pid: lease.worker_pid,
                worker_start_time_ticks: lease.worker_start_time_ticks,
                route: route.to_string(),
            };
            signer
                .verify(&hello.grant, &expect, super::grant::now_ms())
                .map_err(|error| error.to_string())?;
        }
        _ => {
            if !hello_seen {
                return Err(format!("route `{route}` used before the handshake"));
            }
        }
    }
    if frame.task_id() != Some(lease.task_id.as_str()) {
        return Err("frame is addressed to a different task".to_string());
    }
    if let WorkerFrame::Approval {
        correlation_id,
        exchange,
        ..
    } = frame
    {
        if *correlation_id == 0 {
            return Err("approval correlation id is invalid".to_string());
        }
        if !exchange.is_valid() {
            return Err("approval exchange nonce is invalid".to_string());
        }
    }
    if Instant::now() > lease.deadline {
        return Err("worker lease has expired".to_string());
    }
    if !crate::proc::is_pid_alive(lease.worker_pid) {
        return Err("worker process is gone".to_string());
    }
    Ok(())
}

/// Broker-side permission mediation for one worker ask.
///
/// Every identity that decides the outcome — owner, session, task — is
/// read from `lease`, which came from the authenticated grant. The
/// worker contributes only the verb and the scope it was denied, and
/// both are re-validated here: an unknown verb, an unusable scope or a
/// task with no session is refused rather than filed. There is no route
/// to decide a request, to name another session or owner, or to obtain
/// a capability; the strongest possible answer is "one exactly-matching
/// approved grant existed and has now been spent".
fn mediate_approval(used: &mut u32, lease: &Lease, ask: &ApprovalAsk) -> ApprovalReply {
    *used = used.saturating_add(1);
    if *used > protocol::MAX_APPROVAL_ASKS {
        return refuse(
            lease,
            ask,
            "permission-mediation budget exhausted for this task",
        );
    }
    let Some(session_id) = lease.session_id.as_deref().filter(|id| !id.is_empty()) else {
        return refuse(
            lease,
            ask,
            "task has no session; permission mediation is unavailable",
        );
    };
    let Some(verb) = crate::caps::Verb::parse(ask.verb()) else {
        return refuse(lease, ask, "unknown capability verb");
    };
    let (cap, risk) = match crate::approvals::canonical_capability(verb, ask.scope().clone()) {
        Ok(capability) => capability,
        Err(error) => return refuse(lease, ask, &error),
    };
    let scope = &cap.scope;
    let execution = lease.approval_identity();

    match ask {
        ApprovalAsk::Consume { .. } => {
            match crate::approvals::redeem_matching_worker_grant_for_owner_operation(
                session_id,
                verb,
                scope,
                lease.owner_uid,
                &execution,
                ask.operation_digest(),
            ) {
                Ok(Some(grant)) => {
                    let lease_remaining = lease.deadline.saturating_duration_since(Instant::now());
                    match crate::clawd::authority::authorize_worker_approval(
                        lease.owner_uid,
                        &lease.task_id,
                        session_id,
                        lease.worker_pid,
                        lease.worker_start_time_ticks,
                        &lease.approval_nonce,
                        lease_remaining,
                        ask.operation_digest(),
                        &grant,
                    ) {
                        Ok(authority_grant) => {
                            let authority_ref = authority_grant.id.audit_ref();
                            audit_approval(
                                lease,
                                verb,
                                scope,
                                risk,
                                ask.operation_digest(),
                                "granted",
                                Some(&grant),
                                Some(&authority_ref),
                            );
                            ApprovalReply::Granted
                        }
                        Err(error) => {
                            tracing::warn!(
                                task = %lease.task_id,
                                error = %error,
                                "approved capability could not be bound to the worker"
                            );
                            refuse(lease, ask, "approved capability could not be bound to this worker")
                        }
                    }
                }
                Ok(None) if lease.consent_context == ConsentContext::Unattended => {
                    refuse(
                        lease,
                        ask,
                        "unattended tasks cannot request interactive consent; delegate the exact capability when scheduling the task",
                    )
                }
                Ok(None) => ApprovalReply::Pending { request_id: None },
                Err(error) => {
                    tracing::warn!(task = %lease.task_id, error = %error, "approval lookup failed");
                    refuse(lease, ask, "consent store is unavailable")
                }
            }
        }
        ApprovalAsk::Request { .. } => {
            if lease.consent_context == ConsentContext::Unattended {
                return refuse(
                    lease,
                    ask,
                    "unattended tasks cannot request interactive consent; delegate the exact capability when scheduling the task",
                );
            }
            let existing = crate::approvals::find_pending_worker_request_for_operation(
                session_id,
                &cap,
                lease.owner_uid,
                &execution,
                ask.operation_digest(),
            );
            if let Some(request) = existing {
                return ApprovalReply::Pending {
                    request_id: Some(request.id),
                };
            }
            let label = crate::caps::lookup_meta(verb)
                .map(|meta| meta.label.current().to_string())
                .unwrap_or_else(|| verb.as_str().to_string());
            // Reason text is composed here from the catalog and the
            // canonical scope; no worker-authored string is persisted.
            match crate::approvals::submit_worker_request_for_operation(
                verb,
                scope.clone(),
                session_id,
                format!("{label}: {scope}"),
                Some("agentd-worker".to_string()),
                lease.owner_uid,
                lease.task_id.clone(),
                lease.worker_pid,
                lease.worker_start_time_ticks,
                lease.approval_nonce.clone(),
                lease.approval_expires_at,
                ask.operation_digest(),
            ) {
                Ok(id) => {
                    audit_approval(
                        lease,
                        verb,
                        scope,
                        risk,
                        ask.operation_digest(),
                        "requested",
                        None,
                        None,
                    );
                    ApprovalReply::Pending {
                        request_id: Some(id),
                    }
                }
                Err(error) => {
                    tracing::warn!(task = %lease.task_id, error = %error, "approval submit failed");
                    refuse(lease, ask, "consent store is unavailable")
                }
            }
        }
    }
}

fn refuse(lease: &Lease, ask: &ApprovalAsk, message: &str) -> ApprovalReply {
    if let Some(verb) = crate::caps::Verb::parse(ask.verb()) {
        if let Ok(risk) = crate::approvals::capability_risk(verb, ask.scope()) {
            audit_approval(
                lease,
                verb,
                ask.scope(),
                risk,
                ask.operation_digest(),
                "refused",
                None,
                None,
            );
        }
    }
    tracing::warn!(
        task = %lease.task_id,
        owner_uid = lease.owner_uid,
        verb = %crate::audit_policy::safe_identity(ask.verb()),
        "refusing agent worker permission mediation: {message}"
    );
    ApprovalReply::Refused {
        message: message.to_string(),
    }
}

fn audit_approval(
    lease: &Lease,
    verb: crate::caps::Verb,
    scope: &crate::caps::Scope,
    risk: crate::caps::Risk,
    operation_digest: Option<&str>,
    action: &'static str,
    grant: Option<&crate::approvals::ConsumedGrant>,
    authority_grant: Option<&crate::clawd::authority::GrantRef>,
) {
    crate::clawd::audit::record_worker_approval(
        &lease.task_id,
        lease.owner_uid,
        lease.worker_pid,
        lease.worker_start_time_ticks,
        &lease.approval_nonce,
        lease.session_id.as_deref().unwrap_or_default(),
        verb.as_str(),
        scope,
        risk,
        lease.consent_context,
        operation_digest,
        action,
        grant.map(|grant| grant.reference.as_str()),
        grant.map(|grant| grant.generation),
        authority_grant,
    );
}

fn check_hello(hello: &WorkerHello, lease: &Lease) -> Result<(), String> {
    check_hello_with(hello, lease, spawn::broker_is_root())
}

fn check_hello_with(
    hello: &WorkerHello,
    lease: &Lease,
    enforce_group_isolation: bool,
) -> Result<(), String> {
    if hello.protocol != protocol::PROTOCOL_VERSION {
        return Err(format!(
            "agentd protocol mismatch: worker speaks v{}, clawd speaks v{}; \
             reinstall so clawd and claw-agentd come from the same build",
            hello.protocol,
            protocol::PROTOCOL_VERSION
        ));
    }
    if hello.pid != lease.worker_pid {
        return Err(format!(
            "agent worker reported pid {} but the kernel reports {}",
            hello.pid, lease.worker_pid
        ));
    }
    if hello.uid != lease.owner_uid || hello.euid != lease.owner_uid {
        return Err(format!(
            "agent worker is running as uid {} / euid {}, expected {}",
            hello.uid, hello.euid, lease.owner_uid
        ));
    }
    if hello.gid != lease.execution_gid || hello.egid != lease.execution_gid {
        return Err(format!(
            "agent worker is running as gid {} / egid {}, expected isolated gid {}",
            hello.gid, hello.egid, lease.execution_gid
        ));
    }
    if hello.uid == 0 || hello.euid == 0 {
        return Err("agent worker is still running as root".to_string());
    }
    // A root broker forces `setgroups(0, NULL)` before the uid drop, so
    // any surviving group means the drop was defeated. An unprivileged
    // dev/test supervisor cannot clear them in the first place.
    if enforce_group_isolation && !hello.supplementary_groups.is_empty() {
        return Err("agent worker retained supplementary groups".to_string());
    }
    if !hello.no_new_privs {
        return Err("agent worker did not set PR_SET_NO_NEW_PRIVS".to_string());
    }
    if hello.dumpable {
        return Err("agent worker remained dumpable after startup".to_string());
    }
    Ok(())
}

/// Forward the worker's runtime audit into the root-owned `clawd` log.
/// The session is pinned to the lease, so a worker cannot annotate a
/// session it was not granted.
fn record_worker_audit(lease: &Lease, record: &RuntimeAuditRecord) {
    let Some(session_id) = lease.session_id.as_deref() else {
        return;
    };
    if record.session_id() != session_id {
        tracing::warn!(
            task = %lease.task_id,
            "discarding agent worker audit record for an unleased session"
        );
        return;
    }
    crate::clawd::audit::record_worker_runtime(&lease.task_id, lease.owner_uid, record);
}

fn broker_session_info(
    session_id: &str,
) -> Result<(Option<crate::proc::SessionInfo>, ConsentContext), String> {
    let sid = session_id
        .parse::<crate::session::SessionId>()
        .map_err(|error| error.to_string())?;
    if !crate::session::session_dir(&sid).exists() {
        return Ok((None, ConsentContext::Unattended));
    }
    let context = crate::clawd::session_scope::consent_context(&sid)?;
    crate::clawd::session_scope::trusted_session_info(&sid, "claw-agentd")
        .map(|session| (Some(session), context))
}

async fn send(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &BrokerFrame,
) -> Result<(), String> {
    let encoded = protocol::encode(frame)?;
    writer
        .write_all(encoded.as_bytes())
        .await
        .map_err(|error| format!("write agentd frame: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("flush agentd frame: {error}"))
}

async fn send_assignment(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    assignment: Assignment,
) -> Result<(), String> {
    let encoded = protocol::encode(&BrokerFrame::Assign(Box::new(assignment)))?;
    // UnixStream is unbuffered. Once write_all succeeds, the complete framed
    // assignment is deliverable and every later failure is terminal.
    writer
        .write_all(encoded.as_bytes())
        .await
        .map_err(|error| format!("write agentd assignment: {error}"))
}

async fn reap(child: &mut tokio::process::Child, pid: u32) {
    // The worker leads its own session and process group, so this ends
    // any App, MCP server or shell it started rather than leaving them
    // reparented to init. Safe while the child is still unreaped: its
    // pid — and therefore its process-group id — cannot be recycled.
    let _ = child.start_kill();
    unsafe {
        spawn::terminate_worker_group(pid, libc::SIGKILL);
    }
    match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
        Ok(Ok(status)) => {
            tracing::debug!(worker_pid = pid, %status, "agent worker reaped");
        }
        Ok(Err(error)) => {
            tracing::warn!(worker_pid = pid, error = %error, "failed to reap agent worker");
        }
        Err(_) => {
            tracing::warn!(
                worker_pid = pid,
                "agent worker did not exit before the reap deadline"
            );
        }
    }
}

async fn reap_extension_host(
    host: &mut crate::extension_host::spawn::SpawnedExtensionHost,
) -> Result<(), String> {
    let cleanup = host.cgroup.cleanup().await;
    let _ = host.child.start_kill();
    let reaped = match tokio::time::timeout(SHUTDOWN_GRACE, host.child.wait()).await {
        Ok(Ok(status)) => {
            tracing::debug!(host_pid = host.pid, %status, "extension host reaped");
            Ok(())
        }
        Ok(Err(error)) => Err(format!("reap extension host {}: {error}", host.pid)),
        Err(_) => Err(format!(
            "extension host {} did not exit before the reap deadline",
            host.pid
        )),
    };
    match (cleanup, reaped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(cleanup), Ok(())) => Err(cleanup),
        (Ok(()), Err(reap)) => Err(reap),
        (Err(cleanup), Err(reap)) => Err(format!("{cleanup}; {reap}")),
    }
}

fn drain_worker_output(child: &mut tokio::process::Child, task_id: &str) {
    if let Some(stdout) = child.stdout.take() {
        let task_id = task_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(task = %task_id, "agent worker: {line}");
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let task_id = task_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(task = %task_id, "agent worker: {line}");
            }
        });
    }
}

fn drain_extension_output(child: &mut tokio::process::Child, task_id: &str) {
    if let Some(stdout) = child.stdout.take() {
        let task_id = task_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(
                    task = %task_id,
                    "extension host: {}",
                    bounded_log_line(&line)
                );
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let task_id = task_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(
                    task = %task_id,
                    "extension host: {}",
                    bounded_log_line(&line)
                );
            }
        });
    }
}

fn bounded_log_line(line: &str) -> String {
    line.chars().take(4096).collect()
}

fn finish_error(store: &Store, job: Job, message: &str) {
    tracing::warn!(task = %job.id, "agent task rejected: {message}");
    if let Err(error) = store.finish(job, FinishOutcome::Error(message.to_string())) {
        tracing::warn!(error = %error, "failed to persist agent task rejection");
    }
}

/// Retryable failure: hand the task back to the queue, or fail it once
/// it has burned through its recovery budget.
fn release_or_fail(store: &Store, job: &Job, reason: &str) {
    match store.release_for_retry(&job.id, reason) {
        Ok(Some(released)) => tracing::warn!(
            task = %job.id,
            status = released.status.as_str(),
            "agent task released after worker failure: {reason}"
        ),
        Ok(None) => tracing::warn!(task = %job.id, "agent task was already resolved: {reason}"),
        Err(error) => {
            tracing::warn!(task = %job.id, error = %error, "failed to release agent task")
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/supervisor.rs"
    ));
}

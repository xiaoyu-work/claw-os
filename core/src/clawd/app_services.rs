use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};

use crate::agent::tools::app_gateway::McpCallContext;
use crate::agentd::supervisor::BrokerContext;
use crate::caps::manifest::McpLifecycle;
use crate::caps::{CapSet, Role};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::BrokerError;
use crate::clawd::state::DaemonState;
use crate::extension_host::protocol::HostPurpose;
use crate::provenance::runtime::PackageRef;

const SERVICE_LEASE: Duration = Duration::from_secs(10 * 60 * 60);
const LAZY_IDLE: Duration = Duration::from_secs(5 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const GLOBAL_SERVICE_LIMIT: usize =
    crate::extension_host::identity::SERVICE_IDENTITY_COUNT as usize;
const OWNER_SERVICE_LIMIT: usize = 4;
const RESTART_WINDOW: Duration = Duration::from_secs(5 * 60);
const RESTART_LIMIT: usize = 3;

#[derive(Clone, Debug)]
pub(crate) struct AppCallAuthorization {
    pub owner_uid: u32,
    pub app_id: String,
    pub tool: String,
    pub caps: CapSet,
    pub context: McpCallContext,
    pub capability_generation: String,
    pub package: PackageRef,
    pub service_host_session_id: String,
    pub service_host_pid: u32,
    pub service_host_start_time_ticks: Option<u64>,
    pub service_extension_uid: u32,
    pub action_digest: String,
    pub expires_at_ms: u64,
}

#[derive(Debug)]
pub(crate) struct PreparedAppServiceCall {
    pub owner_uid: u32,
    pub app_id: String,
    pub tool: String,
    pub arguments: Value,
    pub context: McpCallContext,
    pub capability_generation: String,
    pub package: PackageRef,
    pub caps: CapSet,
    pub placement: crate::agent::tools::cos_apps_session::CallPlacement,
    pub authorized_mounts: Vec<crate::worker::AuthorizedMount>,
    pub lifecycle: McpLifecycle,
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ServiceKey {
    owner_uid: u32,
    app_id: String,
}

#[derive(Clone)]
struct RuntimeSpec {
    owner_uid: u32,
    app_id: String,
    package: PackageRef,
    lifecycle: McpLifecycle,
}

#[derive(Debug)]
struct RuntimeStartError {
    message: String,
    counts_as_failure: bool,
}

impl RuntimeStartError {
    fn admission(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            counts_as_failure: false,
        }
    }

    fn host(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            counts_as_failure: true,
        }
    }
}

impl std::fmt::Display for RuntimeStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for RuntimeStartError {
    fn from(message: String) -> Self {
        Self::host(message)
    }
}

struct ServiceSlot {
    runtime: Option<ServiceRuntime>,
    lifecycle: McpLifecycle,
    last_used: Instant,
    failures: VecDeque<Instant>,
}

impl ServiceSlot {
    fn new(lifecycle: McpLifecycle) -> Self {
        Self {
            runtime: None,
            lifecycle,
            last_used: Instant::now(),
            failures: VecDeque::new(),
        }
    }

    fn record_failure(&mut self) {
        let cutoff = Instant::now() - RESTART_WINDOW;
        self.failures.push_back(Instant::now());
        while self
            .failures
            .front()
            .is_some_and(|failure| *failure < cutoff)
        {
            self.failures.pop_front();
        }
    }

    fn record_host_exit(&mut self) {
        self.record_failure();
    }

    fn record_start_failure(&mut self, error: &RuntimeStartError) {
        if error.counts_as_failure {
            self.record_failure();
        }
    }

    fn may_restart(&mut self) -> bool {
        let cutoff = Instant::now() - RESTART_WINDOW;
        while self
            .failures
            .front()
            .is_some_and(|failure| *failure < cutoff)
        {
            self.failures.pop_front();
        }
        self.failures.len() < RESTART_LIMIT
    }
}

struct CapacityLease {
    manager: Weak<AppServiceManager>,
    owner_uid: u32,
    _global: OwnedSemaphorePermit,
}

impl Drop for CapacityLease {
    fn drop(&mut self) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        if let Ok(mut counts) = manager.owner_counts.lock() {
            if let Some(count) = counts.get_mut(&self.owner_uid) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    counts.remove(&self.owner_uid);
                }
            }
        };
    }
}

struct ServiceRuntime {
    host: crate::extension_host::spawn::SpawnedExtensionHost,
    identity: Option<crate::extension_host::identity::ExtensionIdentityLease>,
    extension_uid: u32,
    lease: Arc<crate::extension_host::broker::ExtensionLease>,
    broker_task: tokio::task::JoinHandle<()>,
    client: Option<Arc<crate::extension_host::client::ExtensionHostClient>>,
    host_session_id: String,
    package: PackageRef,
    expires_at_ms: u64,
    always_on_ready: bool,
    _capacity: CapacityLease,
}

fn host_fault_requires_retirement(
    category: crate::extension_host::protocol::ExtensionErrorCategory,
) -> bool {
    category != crate::extension_host::protocol::ExtensionErrorCategory::RemoteCallFailure
}

fn child_process_exited(
    child: &mut tokio::process::Child,
    pid: u32,
    start_time_ticks: Option<u64>,
) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => crate::proc::read_start_time_ticks_pub(pid) != start_time_ticks,
        Err(error) => {
            tracing::warn!(host_pid = pid, %error, "failed to poll App service host");
            true
        }
    }
}

impl ServiceRuntime {
    fn host_exited(&mut self) -> bool {
        child_process_exited(
            &mut self.host.child,
            self.host.pid,
            self.host.start_time_ticks,
        )
    }
}

pub struct AppServiceManager {
    broker: BrokerContext,
    slots: AsyncMutex<HashMap<ServiceKey, Arc<AsyncMutex<ServiceSlot>>>>,
    capacity: Arc<Semaphore>,
    owner_counts: Mutex<HashMap<u32, usize>>,
}

impl AppServiceManager {
    pub fn new(broker: BrokerContext) -> Arc<Self> {
        Arc::new(Self {
            broker,
            slots: AsyncMutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(GLOBAL_SERVICE_LIMIT)),
            owner_counts: Mutex::new(HashMap::new()),
        })
    }

    pub fn spawn_sweeper(
        self: &Arc<Self>,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                manager.sweep().await;
                tokio::time::sleep(SWEEP_INTERVAL).await;
            }
            manager.stop_all().await;
        })
    }

    pub(crate) async fn call(
        self: &Arc<Self>,
        prepared: PreparedAppServiceCall,
    ) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, BrokerError> {
        if prepared.deadline_ms <= crate::agentd::grant::now_ms() {
            return Err(BrokerError::authorization(
                "App service call authorization expired before dispatch",
            ));
        }
        let current = current_app(&prepared.app_id).map_err(BrokerError::authorization)?;
        if PackageRef::of(
            current
                .require_verified()
                .map_err(BrokerError::authorization)?,
        ) != prepared.package
            || current
                .manifest
                .mcp
                .as_ref()
                .map(|service| service.lifecycle)
                != Some(prepared.lifecycle)
        {
            return Err(BrokerError::authorization(
                "App package or lifecycle changed before service dispatch",
            ));
        }
        if prepared.lifecycle == McpLifecycle::WhileAppRunning
            && !owner_app_is_running(prepared.owner_uid, &prepared.app_id)
                .await
                .map_err(BrokerError::unavailable)?
        {
            return Err(BrokerError::authorization(
                "App service requires its desktop App to be running",
            ));
        }
        let key = ServiceKey {
            owner_uid: prepared.owner_uid,
            app_id: prepared.app_id.clone(),
        };
        let slot =
            {
                let mut slots = self.slots.lock().await;
                Arc::clone(slots.entry(key).or_insert_with(|| {
                    Arc::new(AsyncMutex::new(ServiceSlot::new(prepared.lifecycle)))
                }))
            };
        let mut slot = slot.lock().await;
        slot.lifecycle = prepared.lifecycle;
        let (retire_runtime, host_exited) =
            slot.runtime.as_mut().map_or((false, false), |runtime| {
                let package_changed = runtime.package != prepared.package;
                let lease_expiring = runtime.expires_at_ms
                    <= crate::agentd::grant::now_ms()
                        .saturating_add(crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS);
                let host_exited = !package_changed && !lease_expiring && runtime.host_exited();
                (
                    package_changed || lease_expiring || host_exited,
                    host_exited,
                )
            });
        if retire_runtime {
            if host_exited {
                slot.record_host_exit();
            }
            if let Some(runtime) = slot.runtime.take() {
                stop_runtime(runtime).await;
            }
        }
        if slot.runtime.is_none() {
            if !slot.may_restart() {
                return Err(BrokerError::unavailable(
                    "App service restart budget is exhausted",
                ));
            }
            let spec = RuntimeSpec {
                owner_uid: prepared.owner_uid,
                app_id: prepared.app_id.clone(),
                package: prepared.package.clone(),
                lifecycle: prepared.lifecycle,
            };
            self.evict_for_capacity(spec.owner_uid).await;
            match self.start_runtime(&spec).await {
                Ok(runtime) => slot.runtime = Some(runtime),
                Err(error) => {
                    slot.record_start_failure(&error);
                    return Err(BrokerError::unavailable(error.to_string()));
                }
            }
        }
        let should_warm = prepared.placement
            == crate::agent::tools::cos_apps_session::CallPlacement::Reusable
            || (prepared.lifecycle == McpLifecycle::AlwaysOn
                && slot
                    .runtime
                    .as_ref()
                    .is_some_and(|runtime| !runtime.always_on_ready));
        if should_warm {
            let warm_timeout = prepared
                .context
                .remaining(Duration::from_millis(
                    crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS,
                ))
                .map_err(BrokerError::authorization)?;
            let warm = {
                let runtime = slot
                    .runtime
                    .as_ref()
                    .ok_or_else(|| BrokerError::unavailable("App service failed to start"))?;
                runtime
                    .client
                    .as_ref()
                    .ok_or_else(|| {
                        BrokerError::unavailable("App service controller is unavailable")
                    })?
                    .warm_app(prepared.app_id.clone(), warm_timeout)
                    .await
            };
            match warm {
                Ok(()) => {
                    if let Some(runtime) = slot.runtime.as_mut() {
                        runtime.always_on_ready = true;
                    }
                }
                Err(error) if host_fault_requires_retirement(error.category()) => {
                    slot.record_failure();
                    if let Some(runtime) = slot.runtime.take() {
                        stop_runtime(runtime).await;
                    }
                    return Err(BrokerError::unavailable(format!(
                        "App service warm-up failed: {error}"
                    )));
                }
                Err(error) => {
                    if let Some(runtime) = slot.runtime.as_mut() {
                        runtime.always_on_ready = false;
                    }
                    return Err(BrokerError::execution(format!(
                        "App service warm-up failed: {error}"
                    )));
                }
            }
        }
        let runtime = slot
            .runtime
            .as_ref()
            .ok_or_else(|| BrokerError::unavailable("App service failed to start"))?;
        let authorization_expiry = prepared.deadline_ms;
        let authorization = self
            .broker
            .state
            .issue_app_authorization(AppCallAuthorization {
                owner_uid: prepared.owner_uid,
                app_id: prepared.app_id.clone(),
                tool: prepared.tool.clone(),
                caps: prepared.caps,
                context: prepared.context.clone(),
                capability_generation: prepared.capability_generation,
                package: prepared.package,
                service_host_session_id: runtime.host_session_id.clone(),
                service_host_pid: runtime.host.pid,
                service_host_start_time_ticks: runtime.host.start_time_ticks,
                service_extension_uid: runtime.extension_uid,
                action_digest: app_call_action_digest(
                    &prepared.app_id,
                    &prepared.tool,
                    &prepared.arguments,
                    &prepared.context,
                    &prepared.authorized_mounts,
                )
                .map_err(BrokerError::unavailable)?,
                expires_at_ms: authorization_expiry,
            })
            .map_err(BrokerError::unavailable)?;
        let authorization_guard = AuthorizationGuard {
            state: self.broker.state.clone(),
            token: authorization.clone(),
        };
        let timeout = prepared
            .context
            .remaining(Duration::from_millis(
                crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS,
            ))
            .map_err(BrokerError::authorization)?;
        let result = runtime
            .client
            .as_ref()
            .ok_or_else(|| BrokerError::unavailable("App service controller is unavailable"))?
            .call_authorized_app(
                prepared.app_id,
                prepared.tool,
                prepared.arguments,
                prepared.authorized_mounts,
                authorization,
                prepared.context,
                timeout,
            )
            .await;
        drop(authorization_guard);
        slot.last_used = Instant::now();
        match result {
            Ok(result) => Ok(result),
            Err(error) if host_fault_requires_retirement(error.category()) => {
                slot.record_failure();
                if let Some(runtime) = slot.runtime.take() {
                    stop_runtime(runtime).await;
                }
                Err(BrokerError::indeterminate(format!(
                    "App service call outcome is uncertain: {error}"
                )))
            }
            Err(error) => {
                if let Some(runtime) = slot.runtime.as_mut() {
                    runtime.always_on_ready = false;
                }
                Err(BrokerError::execution(format!(
                    "App service call failed: {error}"
                )))
            }
        }
    }

    async fn start_runtime(
        self: &Arc<Self>,
        spec: &RuntimeSpec,
    ) -> Result<ServiceRuntime, RuntimeStartError> {
        let app = current_app(&spec.app_id).map_err(RuntimeStartError::admission)?;
        let package = PackageRef::of(
            app.require_verified()
                .map_err(RuntimeStartError::admission)?,
        );
        if package != spec.package {
            return Err(RuntimeStartError::admission(
                "App package changed before service startup",
            ));
        }
        let lifecycle = app
            .manifest
            .mcp
            .as_ref()
            .ok_or_else(|| RuntimeStartError::admission("App no longer exposes an MCP service"))?
            .lifecycle;
        if lifecycle != spec.lifecycle {
            return Err(RuntimeStartError::admission(
                "App lifecycle changed before service startup",
            ));
        }
        if lifecycle == McpLifecycle::WhileAppRunning
            && !owner_app_is_running(spec.owner_uid, &spec.app_id)
                .await
                .map_err(RuntimeStartError::admission)?
        {
            return Err(RuntimeStartError::admission(
                "App service requires its desktop App to be running",
            ));
        }
        let service =
            app.manifest.mcp.as_ref().ok_or_else(|| {
                RuntimeStartError::admission("App no longer exposes an MCP service")
            })?;
        let invoke_caps = service
            .tools
            .iter()
            .map(|tool| crate::agent::tools::app_gateway::invoke_cap(&spec.app_id, &tool.name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(RuntimeStartError::admission)?;
        let standing_caps = CapSet::from_caps(invoke_caps);
        let service_generation =
            crate::agent::tools::exposure::capability_generation(&standing_caps);
        let owner = crate::agentd::spawn::resolve_identity(spec.owner_uid)
            .map_err(RuntimeStartError::admission)?;
        let isolation = crate::agentd::spawn::ExecutionIsolation::capture(
            &self.broker.primary_socket,
            owner.uid,
            self.broker.isolated_execution_gid,
        )
        .map_err(RuntimeStartError::admission)?;
        let approved_paths = crate::agentd::supervisor::extension_approved_paths(&owner, None)
            .map_err(RuntimeStartError::admission)?;
        let containment = self
            .broker
            .extension_containment()
            .map_err(RuntimeStartError::admission)?;
        let capacity = self
            .acquire_capacity(spec.owner_uid)
            .map_err(RuntimeStartError::admission)?;
        let identity_pool = self
            .broker
            .extension_identity_pool()
            .map_err(RuntimeStartError::admission)?;
        let mut identity = identity_pool
            .acquire(owner.uid, HostPurpose::AppService)
            .map_err(RuntimeStartError::admission)?;
        let extension = identity.identity().clone();
        if let Err(error) = identity.begin_task(owner.uid) {
            return Err(join_cleanup_error(error, identity.release()).into());
        }
        let task_name = crate::extension_host::spawn::HostPaths::new_task_name();
        if let Err(error) = identity.record_task(owner.uid, &task_name) {
            let release = identity.release();
            return Err(join_cleanup_error(error, release).into());
        }
        let paths = match crate::extension_host::spawn::HostPaths::create_named(&owner, &task_name)
        {
            Ok(paths) => paths,
            Err(error) => {
                let cleanup =
                    crate::extension_host::spawn::HostPaths::recover(owner.uid, &task_name);
                let release = if cleanup.is_ok() {
                    identity.release()
                } else {
                    Err("identity retained because App service path cleanup failed".to_string())
                };
                return Err(join_cleanup_error(join_cleanup_error(error, cleanup), release).into());
            }
        };
        let listener = match crate::extension_host::broker::bind_listener(
            &paths,
            extension.uid,
            isolation.execution_gid(),
        ) {
            Ok(listener) => listener,
            Err(error) => {
                let cleanup = paths.cleanup();
                let release = if cleanup.is_ok() {
                    identity.release()
                } else {
                    Err("identity retained because App service path cleanup failed".to_string())
                };
                return Err(join_cleanup_error(join_cleanup_error(error, cleanup), release).into());
            }
        };
        if let Err(error) =
            crate::storage::install_routed_extension_reader(owner.uid, extension.uid)
        {
            drop(listener);
            let cleanup =
                cleanup_service_allocation(identity, owner.uid, extension.uid, &paths, false);
            return Err(join_cleanup_error(error, cleanup).into());
        }

        let host_session_id = format!("app-service-{}", uuid::Uuid::new_v4().simple());
        let service_id = uuid::Uuid::new_v4().simple().to_string();
        let lease_nonce = uuid::Uuid::new_v4().simple().to_string();
        let expires_at_ms =
            crate::agentd::grant::now_ms().saturating_add(SERVICE_LEASE.as_millis() as u64);
        let launch = crate::extension_host::spawn::HostLaunchSpec {
            purpose: HostPurpose::AppService,
            lease_id: service_id.clone(),
            authority_session_id: Some(host_session_id.clone()),
            app_id: Some(spec.app_id.clone()),
            host_session_id: Some(host_session_id.clone()),
            controller_uid: unsafe { libc::geteuid() as u32 },
            controller_gid: unsafe { libc::getegid() as u32 },
            controller_pid: std::process::id(),
            controller_start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            package: Some(package.clone()),
        };
        let cleanup_paths = paths.clone();
        let mut host = match crate::extension_host::spawn::spawn_host(
            &owner,
            &extension,
            &isolation,
            &containment,
            &launch,
            &lease_nonce,
            expires_at_ms,
            &service_generation,
            approved_paths,
            Vec::new(),
            paths,
        ) {
            Ok(host) => host,
            Err(error) => {
                drop(listener);
                let cleanup = cleanup_service_allocation(
                    identity,
                    owner.uid,
                    extension.uid,
                    &cleanup_paths,
                    true,
                );
                return Err(join_cleanup_error(error, cleanup).into());
            }
        };
        drain_host_output(&mut host.child, &service_id);
        let info = crate::proc::SessionInfo {
            session_id: host_session_id.clone(),
            pid: host.pid,
            command: vec!["claw-extension-host".to_string(), service_id.clone()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some(crate::extension_host::protocol::APP_SERVICE_HOST_GROUP.to_string()),
            parent: None,
            workdir: Some(host.paths.control_dir.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: Some(Role::Worker.credential_tier()),
            scope: None,
            priority: None,
            caps: Some(standing_caps),
            transient_caps: None,
            role: Some(Role::Worker.name().to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: host.start_time_ticks,
            client: crate::session::SessionClient::new(
                crate::session::SessionSource::System,
                false,
                true,
            ),
        };
        if let Err(error) = crate::proc::register_session_for_owner(info, owner.uid) {
            let containment = crate::agentd::supervisor::reap_extension_host(&mut host).await;
            let cleanup = if containment.is_ok() {
                cleanup_service_allocation(identity, owner.uid, extension.uid, &cleanup_paths, true)
            } else {
                drop(identity);
                Err("identity retained because App service containment cleanup failed".to_string())
            };
            return Err(join_cleanup_error(
                join_cleanup_error(
                    format!("register App service host session: {error}"),
                    containment,
                ),
                cleanup,
            )
            .into());
        }
        let lease = Arc::new(crate::extension_host::broker::ExtensionLease::new(
            HostPurpose::AppService,
            service_id,
            Some(host_session_id.clone()),
            Some(host_session_id.clone()),
            owner.uid,
            extension.uid,
            isolation.execution_gid(),
            service_generation,
            std::process::id(),
            crate::proc::read_start_time_ticks_pub(std::process::id()),
            host.pid,
            host.start_time_ticks,
            expires_at_ms,
        ));
        let broker_task = tokio::spawn(crate::extension_host::broker::serve(
            listener,
            lease.clone(),
            self.broker.state.clone(),
            self.broker.admission.clone(),
        ));
        let client = match crate::extension_host::client::ExtensionHostClient::connect_controller(
            host.binding.clone(),
        )
        .await
        {
            Ok(client) => client,
            Err(error) => {
                let runtime = ServiceRuntime {
                    host,
                    identity: Some(identity),
                    extension_uid: extension.uid,
                    lease,
                    broker_task,
                    client: None,
                    host_session_id,
                    package,
                    expires_at_ms,
                    always_on_ready: false,
                    _capacity: capacity,
                };
                stop_runtime(runtime).await;
                return Err(error.into());
            }
        };
        Ok(ServiceRuntime {
            host,
            identity: Some(identity),
            extension_uid: extension.uid,
            lease,
            broker_task,
            client: Some(client),
            host_session_id,
            package,
            expires_at_ms,
            always_on_ready: false,
            _capacity: capacity,
        })
    }

    fn acquire_capacity(self: &Arc<Self>, owner_uid: u32) -> Result<CapacityLease, String> {
        let global = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "global App service capacity is exhausted".to_string())?;
        let mut counts = self
            .owner_counts
            .lock()
            .map_err(|_| "App service owner capacity is unavailable".to_string())?;
        let count = counts.entry(owner_uid).or_default();
        if *count >= OWNER_SERVICE_LIMIT {
            return Err("owner App service capacity is exhausted".to_string());
        }
        *count += 1;
        Ok(CapacityLease {
            manager: Arc::downgrade(self),
            owner_uid,
            _global: global,
        })
    }

    async fn evict_for_capacity(self: &Arc<Self>, owner_uid: u32) {
        let owner_full = self
            .owner_counts
            .lock()
            .ok()
            .and_then(|counts| counts.get(&owner_uid).copied())
            .unwrap_or_default()
            >= OWNER_SERVICE_LIMIT;
        if !owner_full && self.capacity.available_permits() > 0 {
            return;
        }
        let entries = self
            .slots
            .lock()
            .await
            .iter()
            .map(|(key, slot)| (key.clone(), Arc::clone(slot)))
            .collect::<Vec<_>>();
        let mut candidate: Option<(Instant, ServiceKey, Arc<AsyncMutex<ServiceSlot>>)> = None;
        for (key, slot) in entries {
            if owner_full && key.owner_uid != owner_uid {
                continue;
            }
            let Ok(slot_guard) = slot.try_lock() else {
                continue;
            };
            if slot_guard.lifecycle != McpLifecycle::Lazy || slot_guard.runtime.is_none() {
                continue;
            }
            if candidate
                .as_ref()
                .is_none_or(|(last_used, _, _)| slot_guard.last_used < *last_used)
            {
                candidate = Some((slot_guard.last_used, key, Arc::clone(&slot)));
            }
        }
        let Some((_, key, slot)) = candidate else {
            return;
        };
        let Ok(mut slot) = slot.try_lock() else {
            return;
        };
        if slot.lifecycle != McpLifecycle::Lazy {
            return;
        }
        if let Some(runtime) = slot.runtime.take() {
            tracing::info!(
                owner = key.owner_uid,
                app = %key.app_id,
                "evicting idle App service under capacity pressure"
            );
            stop_runtime(runtime).await;
        }
    }

    async fn warm_always_on_for_owner(self: &Arc<Self>, owner_uid: u32) {
        for (app_id, app) in crate::apps::discover_verified(&apps_root()) {
            let Some(service) = app.manifest.mcp.as_ref() else {
                continue;
            };
            if service.lifecycle != McpLifecycle::AlwaysOn {
                continue;
            }
            let Ok(verified) = app.require_verified() else {
                continue;
            };
            let spec = RuntimeSpec {
                owner_uid,
                app_id,
                package: PackageRef::of(verified),
                lifecycle: McpLifecycle::AlwaysOn,
            };
            let key = ServiceKey {
                owner_uid,
                app_id: spec.app_id.clone(),
            };
            let slot =
                {
                    let mut slots = self.slots.lock().await;
                    Arc::clone(slots.entry(key.clone()).or_insert_with(|| {
                        Arc::new(AsyncMutex::new(ServiceSlot::new(spec.lifecycle)))
                    }))
                };
            let Ok(mut slot) = slot.try_lock() else {
                continue;
            };
            slot.lifecycle = McpLifecycle::AlwaysOn;
            let (reusable, host_exited) = slot.runtime.as_mut().map_or((false, false), |runtime| {
                let package_changed = runtime.package != spec.package;
                let lease_expired = runtime.expires_at_ms <= crate::agentd::grant::now_ms();
                let host_exited = !package_changed && !lease_expired && runtime.host_exited();
                (
                    !package_changed && !lease_expired && !host_exited,
                    host_exited,
                )
            });
            if !reusable {
                if host_exited {
                    slot.record_host_exit();
                }
                if let Some(runtime) = slot.runtime.take() {
                    stop_runtime(runtime).await;
                }
                if !slot.may_restart() {
                    continue;
                }
                self.evict_for_capacity(owner_uid).await;
                match self.start_runtime(&spec).await {
                    Ok(runtime) => {
                        slot.runtime = Some(runtime);
                        slot.last_used = Instant::now();
                    }
                    Err(error) => {
                        slot.record_start_failure(&error);
                        tracing::error!(
                            owner = key.owner_uid,
                            app = %key.app_id,
                            %error,
                            "failed to start always-on App service"
                        );
                        continue;
                    }
                }
            }
            if slot
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.always_on_ready)
            {
                continue;
            }
            let warm = match slot
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.client.as_ref())
            {
                Some(client) => {
                    client
                        .warm_app(spec.app_id.clone(), Duration::from_secs(60))
                        .await
                }
                None => continue,
            };
            match warm {
                Ok(()) => {
                    if let Some(runtime) = slot.runtime.as_mut() {
                        runtime.always_on_ready = true;
                    }
                    slot.last_used = Instant::now();
                }
                Err(error) if host_fault_requires_retirement(error.category()) => {
                    slot.record_failure();
                    if let Some(runtime) = slot.runtime.take() {
                        stop_runtime(runtime).await;
                    }
                    tracing::error!(
                        owner = key.owner_uid,
                        app = %key.app_id,
                        %error,
                        "always-on App service Host failed during warm-up"
                    );
                }
                Err(error) => {
                    if let Some(runtime) = slot.runtime.as_mut() {
                        runtime.always_on_ready = false;
                    }
                    tracing::warn!(
                        owner = key.owner_uid,
                        app = %key.app_id,
                        %error,
                        "always-on App rejected warm-up"
                    );
                }
            }
        }
    }

    async fn sweep(self: &Arc<Self>) {
        let entries = self
            .slots
            .lock()
            .await
            .iter()
            .map(|(key, slot)| (key.clone(), Arc::clone(slot)))
            .collect::<Vec<_>>();
        let mut owners = super::server::routed_owner_uids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        for (key, slot) in entries {
            owners.insert(key.owner_uid);
            let Ok(mut slot) = slot.try_lock() else {
                continue;
            };
            let lifecycle = slot.lifecycle;
            let idle = lifecycle == McpLifecycle::Lazy && slot.last_used.elapsed() >= LAZY_IDLE;
            let app_stopped = lifecycle == McpLifecycle::WhileAppRunning
                && !owner_app_is_running(key.owner_uid, &key.app_id)
                    .await
                    .unwrap_or(false);
            let current_contract = current_app(&key.app_id).ok().and_then(|app| {
                let package = PackageRef::of(app.require_verified().ok()?);
                let lifecycle = app.manifest.mcp.as_ref()?.lifecycle;
                Some((package, lifecycle))
            });
            let (retire_runtime, host_exited) = match slot.runtime.as_mut() {
                None => (false, false),
                Some(runtime) => {
                    let lease_expired = runtime.expires_at_ms <= crate::agentd::grant::now_ms();
                    let contract_changed =
                        current_contract.as_ref() != Some(&(runtime.package.clone(), lifecycle));
                    let expected_retirement =
                        lease_expired || contract_changed || idle || app_stopped;
                    let host_exited = !expected_retirement && runtime.host_exited();
                    (expected_retirement || host_exited, host_exited)
                }
            };
            if retire_runtime {
                if host_exited {
                    slot.record_host_exit();
                    tracing::warn!(
                        owner = key.owner_uid,
                        app = %key.app_id,
                        "App service host exited unexpectedly"
                    );
                }
                if let Some(runtime) = slot.runtime.take() {
                    stop_runtime(runtime).await;
                }
            }
        }
        for owner_uid in owners {
            self.warm_always_on_for_owner(owner_uid).await;
        }
    }

    async fn stop_all(&self) {
        let slots = self
            .slots
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for slot in slots {
            let mut slot = slot.lock().await;
            if let Some(runtime) = slot.runtime.take() {
                stop_runtime(runtime).await;
            }
        }
    }
}

struct AuthorizationGuard {
    state: DaemonState,
    token: String,
}

impl Drop for AuthorizationGuard {
    fn drop(&mut self) {
        self.state.revoke_app_authorization(&self.token);
    }
}

async fn stop_runtime(mut runtime: ServiceRuntime) {
    runtime.lease.close();
    runtime.broker_task.abort();
    for child in crate::proc::deregister_child_sessions_for_owner(
        &runtime.host_session_id,
        runtime.lease.owner_uid,
    ) {
        crate::clawd::authority::revoke_session_for_owner(&child, runtime.lease.owner_uid);
        crate::provenance::runtime::deregister(runtime.lease.owner_uid, &child);
    }
    crate::proc::deregister_session_for_owner(&runtime.host_session_id, runtime.lease.owner_uid);
    crate::clawd::authority::revoke_session_for_owner(
        &runtime.host_session_id,
        runtime.lease.owner_uid,
    );
    let cleanup = crate::agentd::supervisor::reap_extension_host(&mut runtime.host).await;
    if cleanup.is_ok() {
        let acl = crate::storage::remove_routed_extension_reader(
            runtime.lease.owner_uid,
            runtime.extension_uid,
        );
        let acl = join_cleanup_result(
            acl,
            crate::storage::purge_routed_extension_reader(runtime.extension_uid),
        );
        match acl {
            Ok(()) => {
                if let Some(identity) = runtime.identity.take() {
                    if let Err(error) = identity.release() {
                        tracing::error!(
                            owner_uid = runtime.lease.owner_uid,
                            app = ?runtime.host.binding.app_id,
                            %error,
                            "App service identity remains quarantined after cleanup"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    owner_uid = runtime.lease.owner_uid,
                    app = ?runtime.host.binding.app_id,
                    %error,
                    "App service identity remains quarantined because ACL cleanup failed"
                );
            }
        }
    } else {
        tracing::error!(
            owner_uid = runtime.lease.owner_uid,
            app = ?runtime.host.binding.app_id,
            "App service containment cleanup failed"
        );
    }
}

fn cleanup_service_allocation(
    identity: crate::extension_host::identity::ExtensionIdentityLease,
    owner_uid: u32,
    extension_uid: u32,
    paths: &crate::extension_host::spawn::HostPaths,
    acl_registered: bool,
) -> Result<(), String> {
    let acl = if acl_registered {
        crate::storage::remove_routed_extension_reader(owner_uid, extension_uid)
    } else {
        Ok(())
    };
    let acl = join_cleanup_result(
        acl,
        crate::storage::purge_routed_extension_reader(extension_uid),
    );
    let cleanup = join_cleanup_result(acl, paths.cleanup());
    if cleanup.is_err() {
        drop(identity);
        return cleanup;
    }
    identity.release()
}

fn join_cleanup_result(
    first: Result<(), String>,
    second: Result<(), String>,
) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(first), Ok(())) => Err(first),
        (Ok(()), Err(second)) => Err(second),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

fn drain_host_output(child: &mut tokio::process::Child, service_id: &str) {
    if let Some(stdout) = child.stdout.take() {
        let service_id = service_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(service = %service_id, %line, "App service host stdout");
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let service_id = service_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(service = %service_id, %line, "App service host stderr");
            }
        });
    }
}

fn current_app(app_id: &str) -> Result<crate::apps::App, String> {
    crate::apps::find_verified_fresh(&apps_root(), app_id)
}

fn apps_root() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".to_string()),
    )
}

async fn owner_app_is_running(owner_uid: u32, app_id: &str) -> Result<bool, String> {
    let home = crate::paths::verified_home_for_uid(owner_uid)?;
    let app_id = app_id.to_string();
    Ok(
        crate::paths::with_user_override(owner_uid, home, async move {
            crate::proc::registry_sessions().into_iter().any(|session| {
                session.app_id.as_deref() == Some(app_id.as_str())
                    && session.group.as_deref() == Some("app")
                    && session.pid > 1
                    && session.start_time_ticks.is_some()
                    && crate::proc::read_start_time_ticks_pub(session.pid)
                        == session.start_time_ticks
            })
        })
        .await,
    )
}

fn join_cleanup_error(error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; cleanup failed: {cleanup}"),
    }
}

pub(crate) fn app_call_action_digest(
    app_id: &str,
    tool: &str,
    arguments: &Value,
    context: &McpCallContext,
    authorized_mounts: &[crate::worker::AuthorizedMount],
) -> Result<String, String> {
    let canonical = serde_json::to_vec(&(app_id, tool, arguments, context, authorized_mounts))
        .map_err(|error| format!("encode App call authorization binding: {error}"))?;
    Ok(crate::crypto::sha256_hex(&canonical))
}

pub async fn call(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    let prepared = super::app_sessions::prepare_app_service_call(params, client).await?;
    let manager = state
        .app_service_manager()
        .ok_or_else(|| BrokerError::unavailable("App service manager is unavailable"))?;
    let result = manager.call(prepared).await?;
    serde_json::to_value(result)
        .map_err(|error| BrokerError::execution(format!("encode App service result: {error}")))
}

/// Authenticated local CLI App invocation (`Access::User`).
///
/// The daemon derives identity, call context, capabilities, package,
/// owner uid and deadline entirely from the peer — the request supplies
/// only the exact App id, tool and arguments. The prepared call is then
/// handed to the same [`AppServiceManager`] path as the private task
/// host, so lifecycle, restart, capacity, ticketing and the launch gate
/// are shared and there is no alternate App process path.
pub async fn cli_call(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, BrokerError> {
    let prepared = super::app_sessions::prepare_cli_app_service_call(params, client).await?;
    let manager = state
        .app_service_manager()
        .ok_or_else(|| BrokerError::unavailable("App service manager is unavailable"))?;
    let result = manager.call(prepared).await?;
    serde_json::to_value(result)
        .map_err(|error| BrokerError::execution(format!("encode App service result: {error}")))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_services.rs"
    ));
}

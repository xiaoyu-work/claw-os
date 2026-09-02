//! Daemon-owned persistent MCP App hosts.
//!
//! One trusted Host is shared by an owner's App services. Each service still
//! runs as its own isolated Host child; sharing applies only to the trusted
//! supervisor and its reserved execution identity.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

use crate::agent::tools::mcp::protocol::CallToolResult;
use crate::agentd::protocol::AppGatewayRequest;
use crate::agentd::spawn::{ExecutionIsolation, WorkerIdentity};
use crate::caps::{CapSet, Role};
use crate::clawd::state::DaemonState;
use crate::clawd::transport::limits::Admission;
use crate::extension_host::broker::ExtensionLease;
use crate::extension_host::client::ExtensionHostClient;
use crate::extension_host::identity::{ExtensionIdentityLease, ExtensionIdentityPool};
use crate::extension_host::protocol::{
    AppInvocationAudit, AuditStage, ExtensionKind, LifecycleAction,
};
use crate::extension_host::spawn::{ContainmentRoot, HostPaths, SpawnedExtensionHost};

const HOST_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const APPROVAL_POLL: Duration = Duration::from_millis(250);
const LAZY_APP_IDLE: Duration = Duration::from_secs(5 * 60);
const OWNER_HOST_IDLE: Duration = Duration::from_secs(10 * 60);
const APP_RESTART_MAX: Duration = Duration::from_secs(60);
const PERSISTENT_BROKER_LEASE: Duration = Duration::from_secs(120);

pub(crate) struct PersistentAppHostManager {
    state: DaemonState,
    admission: Arc<Admission>,
    primary_socket: PathBuf,
    isolated_execution_gid: u32,
    containment: Arc<OnceLock<Result<Arc<ContainmentRoot>, Arc<str>>>>,
    identities: Arc<OnceLock<Result<Arc<ExtensionIdentityPool>, Arc<str>>>>,
    hosts: Mutex<HashMap<u32, Arc<OwnerHostSlot>>>,
}

struct OwnerHostSlot {
    owner_uid: u32,
    owner_home: PathBuf,
    runtime: Mutex<Option<PersistentHostRuntime>>,
    services: Mutex<HashMap<String, AppServiceState>>,
    restart: Mutex<RestartState>,
}

#[derive(Default)]
struct RestartState {
    failures: u32,
    retry_at: Option<Instant>,
}

struct AppServiceState {
    lifecycle: crate::caps::manifest::McpLifecycle,
    tool: String,
    last_used: Instant,
    running: bool,
    failures: u32,
    retry_at: Option<Instant>,
    call_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct AppServiceSnapshot {
    app_id: String,
    lifecycle: crate::caps::manifest::McpLifecycle,
    tool: String,
    last_used: Instant,
    running: bool,
    retry_at: Option<Instant>,
    call_lock: Arc<Mutex<()>>,
}

struct PersistentHostRuntime {
    owner_uid: u32,
    host_session_id: String,
    host: SpawnedExtensionHost,
    client: Arc<ExtensionHostClient>,
    identity: Option<ExtensionIdentityLease>,
    extension_uid: u32,
    lease: Arc<ExtensionLease>,
    broker_task: tokio::task::JoinHandle<()>,
    last_used: Instant,
}

impl PersistentAppHostManager {
    pub(crate) fn new(
        state: DaemonState,
        admission: Arc<Admission>,
        primary_socket: PathBuf,
        isolated_execution_gid: u32,
    ) -> Self {
        Self {
            state,
            admission,
            primary_socket,
            isolated_execution_gid,
            containment: Arc::new(OnceLock::new()),
            identities: Arc::new(OnceLock::new()),
            hosts: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn containment(&self) -> Result<Arc<ContainmentRoot>, String> {
        match self.containment.get_or_init(|| {
            ContainmentRoot::establish()
                .map(Arc::new)
                .map_err(Arc::<str>::from)
        }) {
            Ok(root) => Ok(root.clone()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn identity_pool(&self) -> Result<Arc<ExtensionIdentityPool>, String> {
        match self.identities.get_or_init(|| {
            ExtensionIdentityPool::load(self.isolated_execution_gid).map_err(Arc::<str>::from)
        }) {
            Ok(pool) => Ok(pool.clone()),
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn install_test_identity_pool(&self, pool: Arc<ExtensionIdentityPool>) {
        let _ = self.identities.set(Ok(pool));
    }

    pub(crate) async fn call(
        &self,
        authority: &super::app_sessions::AgentGatewayAuthority,
        request: AppGatewayRequest,
    ) -> Result<CallToolResult, String> {
        request.validate()?;
        let context =
            crate::agent::tools::app_gateway::McpCallContext::for_authenticated_system_agent(
                authority.owner_uid,
                &authority.session_id,
                &authority.task_id,
                Duration::from_millis(request.timeout_ms),
                authority.lease_deadline_ms,
            )?;
        let authorized = loop {
            match super::app_sessions::authorize_agent_gateway_call(
                authority,
                &request.app_id,
                &request.tool,
                &request.arguments,
                &context,
            )
            .await
            {
                Ok(authorized) => break authorized,
                Err(error) if error.audit_class == Some("approval_required") => {
                    let wait = context.remaining(APPROVAL_POLL)?;
                    tokio::time::sleep(wait).await;
                }
                Err(error) => return Err(error.message),
            }
        };
        if authorized.lifecycle == crate::caps::manifest::McpLifecycle::WhileAppRunning
            && !gui_app_is_running(
                authority.owner_uid,
                &authority.owner_home,
                &request.app_id,
                None,
            )
            .await
        {
            return Err(format!(
                "App `{}` MCP service is available only while its desktop App is running",
                request.app_id
            ));
        }
        let (slot, client, binding, host_session_id) = self
            .get_or_start(authority.owner_uid, &authority.owner_home, true)
            .await?;
        let call_lock =
            track_service(&slot, &request.app_id, &request.tool, authorized.lifecycle).await;
        let _call = call_lock.lock().await;
        if authorized.lifecycle == crate::caps::manifest::McpLifecycle::WhileAppRunning
            && !gui_app_is_running(
                authority.owner_uid,
                &authority.owner_home,
                &request.app_id,
                Some(&host_session_id),
            )
            .await
        {
            return Err(format!(
                "App `{}` desktop session ended before its MCP call",
                request.app_id
            ));
        }
        super::app_sessions::verify_agent_gateway_authority(authority)
            .await
            .map_err(|error| error.message)?;
        let gateway_handle = super::app_sessions::issue_gateway_dispatch_grant(
            &binding,
            &request.app_id,
            &request.tool,
            &authorized.arguments,
            &context,
            &authority.capability_generation,
            &authorized.package_digest,
            authorized.target_caps,
        )?;
        let timeout = context.remaining(Duration::from_millis(request.timeout_ms))?;
        let invocation = AppInvocationAudit::new(
            request.app_id.clone(),
            request.tool.clone(),
            authority.capability_generation.clone(),
            context.clone(),
        )?;
        let started = Instant::now();
        let result = client
            .call_persistent_app(
                request.app_id.clone(),
                request.tool,
                authorized.arguments,
                context,
                authority.capability_generation.clone(),
                Some(authorized.package_digest),
                gateway_handle,
                timeout,
            )
            .await;
        record_call(
            authority,
            &binding,
            invocation,
            started.elapsed(),
            result.as_ref().err().map(String::as_str),
        );
        record_service_result(&slot, &request.app_id, result.is_ok()).await;
        result
    }

    async fn get_or_start(
        &self,
        owner_uid: u32,
        owner_home: &std::path::Path,
        mark_used: bool,
    ) -> Result<
        (
            Arc<OwnerHostSlot>,
            Arc<ExtensionHostClient>,
            crate::extension_host::protocol::ExtensionBinding,
            String,
        ),
        String,
    > {
        let slot = self.owner_slot(owner_uid, owner_home).await?;
        let mut runtime = slot.runtime.lock().await;
        let live = runtime.as_mut().is_some_and(|active| {
            crate::proc::read_start_time_ticks_pub(active.host.pid) == active.host.start_time_ticks
                && active.host.child.try_wait().ok().flatten().is_none()
        });
        let stale = (!live).then(|| runtime.take()).flatten();
        if let Some(stale) = stale {
            if let Err(error) = shutdown_runtime(stale).await {
                record_host_failure(&slot).await;
                return Err(format!("stale persistent App Host cleanup failed: {error}"));
            }
            record_host_failure(&slot).await;
        }
        if runtime.is_none() {
            require_host_restart_ready(&slot).await?;
            match self.start_owner_host(owner_uid, owner_home).await {
                Ok(host) => {
                    clear_host_failures(&slot).await;
                    *runtime = Some(host);
                }
                Err(error) => {
                    record_host_failure(&slot).await;
                    return Err(error);
                }
            }
        }
        let active = runtime
            .as_mut()
            .ok_or_else(|| "persistent App Host was not installed".to_string())?;
        if mark_used {
            active.last_used = Instant::now();
        }
        active.lease.renew(PERSISTENT_BROKER_LEASE);
        let result = (
            slot.clone(),
            active.client.clone(),
            active.host.binding.clone(),
            active.host_session_id.clone(),
        );
        drop(runtime);
        self.reconcile_services(&slot, &result.1, &result.3).await;
        Ok(result)
    }

    async fn owner_slot(
        &self,
        owner_uid: u32,
        owner_home: &std::path::Path,
    ) -> Result<Arc<OwnerHostSlot>, String> {
        let slot = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(owner_uid)
                .or_insert_with(|| {
                    Arc::new(OwnerHostSlot {
                        owner_uid,
                        owner_home: owner_home.to_path_buf(),
                        runtime: Mutex::new(None),
                        services: Mutex::new(HashMap::new()),
                        restart: Mutex::new(RestartState::default()),
                    })
                })
                .clone()
        };
        if slot.owner_home != owner_home {
            return Err("persistent App Host owner home changed".to_string());
        }
        Ok(slot)
    }

    pub(crate) async fn recognize_owner(&self, owner_uid: u32, owner_home: &std::path::Path) {
        let slot = match self.owner_slot(owner_uid, owner_home).await {
            Ok(slot) => slot,
            Err(error) => {
                tracing::warn!(owner_uid, error = %error, "could not register App Host owner");
                return;
            }
        };
        merge_declared_services(&slot).await;
        if slot_needs_host(&slot).await {
            if let Err(error) = self.get_or_start(owner_uid, owner_home, false).await {
                tracing::warn!(
                    owner_uid,
                    error = %error,
                    "could not warm owner App services"
                );
            }
        }
    }

    async fn start_owner_host(
        &self,
        owner_uid: u32,
        owner_home: &std::path::Path,
    ) -> Result<PersistentHostRuntime, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("persistent App Host manager requires root".to_string());
        }
        let identity = crate::agentd::spawn::resolve_identity(owner_uid)?;
        if identity.home != owner_home {
            return Err("persistent App Host owner home changed".to_string());
        }
        let isolation = ExecutionIsolation::capture(
            &self.primary_socket,
            owner_uid,
            self.isolated_execution_gid,
        )?;
        let containment = self.containment()?;
        let mut execution_identity = self.identity_pool()?.acquire(owner_uid)?;
        let extension = execution_identity.identity().clone();
        execution_identity.begin_task(owner_uid)?;
        let task_name = HostPaths::new_task_name();
        if let Err(error) = execution_identity.record_task(owner_uid, &task_name) {
            return Err(release_after_error(execution_identity, error));
        }
        let paths = match HostPaths::create_named(&identity, &task_name) {
            Ok(paths) => paths,
            Err(error) => {
                let cleanup = HostPaths::recover(owner_uid, &task_name);
                return Err(cleanup_start_error(error, cleanup, execution_identity));
            }
        };
        let listener = match crate::extension_host::broker::bind_listener(
            &paths,
            extension.uid,
            isolation.execution_gid(),
        ) {
            Ok(listener) => listener,
            Err(error) => {
                return Err(cleanup_start_error(
                    error,
                    paths.cleanup(),
                    execution_identity,
                ));
            }
        };
        if let Err(error) =
            crate::storage::install_routed_extension_reader(owner_uid, extension.uid)
        {
            drop(listener);
            let acl = crate::storage::purge_routed_extension_reader(extension.uid);
            let path = paths.cleanup();
            let cleanup = merge_cleanup(acl, path);
            return Err(cleanup_start_error(error, cleanup, execution_identity));
        }

        let host_session_id = format!("app-host-{}", uuid::Uuid::new_v4().simple());
        let lease_nonce = uuid::Uuid::new_v4().simple().to_string();
        let controller_pid = std::process::id();
        let controller_start_time = crate::proc::read_start_time_ticks_pub(controller_pid);
        let host_caps = CapSet::from_iter([crate::caps::Cap::new(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name("**"),
        )]);
        let capability_generation =
            crate::agent::tools::exposure::capability_generation(&host_caps);
        let approved_paths = persistent_approved_paths(&identity)?;
        let cleanup_paths = paths.clone();
        let mut host = match crate::extension_host::spawn::spawn_persistent_owner_host(
            &identity,
            &extension,
            &isolation,
            &containment,
            &format!("owner-host-{owner_uid}"),
            &host_session_id,
            controller_pid,
            controller_start_time,
            &lease_nonce,
            u64::MAX,
            &capability_generation,
            approved_paths,
            paths,
        ) {
            Ok(host) => host,
            Err(error) => {
                let cleanup = merge_cleanup(
                    crate::storage::purge_routed_extension_reader(extension.uid),
                    cleanup_paths.cleanup(),
                );
                return Err(format!(
                    "{}; identity quarantined until restart recovery verifies containment",
                    merge_error(error, cleanup)
                ));
            }
        };
        drain_host_output(&mut host, owner_uid);

        let info = crate::proc::SessionInfo {
            session_id: host_session_id.clone(),
            pid: host.pid,
            command: vec![
                "claw-extension-host".to_string(),
                format!("owner:{owner_uid}"),
            ],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some(crate::extension_host::protocol::EXTENSION_HOST_GROUP.to_string()),
            parent: None,
            workdir: Some(host.paths.control_dir.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: Some(Role::Worker.credential_tier()),
            scope: None,
            priority: None,
            caps: Some(host_caps),
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
        if let Err(error) = crate::proc::register_session_for_owner(info, owner_uid) {
            let mut cleanup =
                cleanup_started_host(&mut host, owner_uid, extension.uid, Some(&host_session_id))
                    .await;
            if cleanup.is_ok() {
                cleanup = execution_identity.release();
            }
            return Err(merge_error(
                format!("register persistent extension-host session: {error}"),
                cleanup,
            ));
        }

        let lease = Arc::new(ExtensionLease::new(
            format!("owner-host-{owner_uid}"),
            None,
            Some(host_session_id.clone()),
            owner_uid,
            extension.uid,
            isolation.execution_gid(),
            controller_pid,
            controller_start_time,
            host.pid,
            host.start_time_ticks,
            crate::agentd::grant::now_ms()
                .saturating_add(PERSISTENT_BROKER_LEASE.as_millis() as u64),
        ));
        let broker_task = tokio::spawn(crate::extension_host::broker::serve(
            listener,
            lease.clone(),
            self.state.clone(),
            self.admission.clone(),
        ));
        let client = match crate::extension_host::client::connect_persistent_controller(
            host.binding.clone(),
        )
        .await
        {
            Ok(client) => client,
            Err(error) => {
                lease.close();
                broker_task.abort();
                let mut cleanup = cleanup_started_host(
                    &mut host,
                    owner_uid,
                    extension.uid,
                    Some(&host_session_id),
                )
                .await;
                if cleanup.is_ok() {
                    cleanup = execution_identity.release();
                }
                return Err(merge_error(
                    format!("connect persistent App Host: {error}"),
                    cleanup,
                ));
            }
        };
        crate::clawd::audit::record_extension_host_event(
            &format!("owner-host-{owner_uid}"),
            None,
            owner_uid,
            host.pid,
            host.start_time_ticks,
            "attach",
            true,
        );
        Ok(PersistentHostRuntime {
            owner_uid,
            host_session_id,
            host,
            client,
            identity: Some(execution_identity),
            extension_uid: extension.uid,
            lease,
            broker_task,
            last_used: Instant::now(),
        })
    }

    async fn reconcile_services(
        &self,
        slot: &Arc<OwnerHostSlot>,
        client: &Arc<ExtensionHostClient>,
        host_session_id: &str,
    ) {
        merge_declared_services(slot).await;
        let services = service_snapshots(slot).await;
        for service in services {
            let gui_running =
                if service.lifecycle == crate::caps::manifest::McpLifecycle::WhileAppRunning {
                    gui_app_is_running(
                        slot.owner_uid,
                        &slot.owner_home,
                        &service.app_id,
                        Some(host_session_id),
                    )
                    .await
                } else {
                    false
                };
            let desired = service_should_run(&service, gui_running);
            if desired == service.running
                && (!desired || service.lifecycle == crate::caps::manifest::McpLifecycle::Lazy)
            {
                continue;
            }
            if desired
                && service
                    .retry_at
                    .is_some_and(|retry_at| retry_at > Instant::now())
            {
                continue;
            }
            let _call = service.call_lock.lock().await;
            let Some(current) = service_snapshot(slot, &service.app_id).await else {
                continue;
            };
            let gui_running =
                if current.lifecycle == crate::caps::manifest::McpLifecycle::WhileAppRunning {
                    gui_app_is_running(
                        slot.owner_uid,
                        &slot.owner_home,
                        &current.app_id,
                        Some(host_session_id),
                    )
                    .await
                } else {
                    false
                };
            let desired = service_should_run(&current, gui_running);
            if (desired == current.running
                && (!desired || current.lifecycle == crate::caps::manifest::McpLifecycle::Lazy))
                || (desired
                    && current
                        .retry_at
                        .is_some_and(|retry_at| retry_at > Instant::now()))
            {
                continue;
            }
            let result = if desired {
                client
                    .warm_app(current.app_id.clone(), current.tool)
                    .await
                    .map(|_| true)
            } else {
                client
                    .close_app(current.app_id.clone())
                    .await
                    .map(|_| false)
            };
            update_reconcile_result(slot, &current.app_id, result).await;
        }
    }

    pub(crate) async fn sweep(&self) {
        let slots = self
            .hosts
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for slot in slots {
            merge_declared_services(&slot).await;
            let should_run = slot_needs_host(&slot).await;
            let (live, stale, active) = {
                let mut runtime = slot.runtime.lock().await;
                let live = runtime.as_mut().is_some_and(|active| {
                    crate::proc::read_start_time_ticks_pub(active.host.pid)
                        == active.host.start_time_ticks
                        && active.host.child.try_wait().ok().flatten().is_none()
                });
                let stale = (!live).then(|| runtime.take()).flatten();
                let active = runtime.as_ref().map(|active| {
                    active.lease.renew(PERSISTENT_BROKER_LEASE);
                    (active.client.clone(), active.host_session_id.clone())
                });
                (live, stale, active)
            };
            if let Some(stale) = stale {
                if let Err(error) = shutdown_runtime(stale).await {
                    record_host_failure(&slot).await;
                    tracing::error!(
                        owner_uid = slot.owner_uid,
                        error = %error,
                        "crashed persistent App Host cleanup failed"
                    );
                    continue;
                }
                record_host_failure(&slot).await;
            }
            if live {
                if let Some((client, host_session_id)) = active {
                    self.reconcile_services(&slot, &client, &host_session_id)
                        .await;
                }
            } else if should_run {
                if let Err(error) = self
                    .get_or_start(slot.owner_uid, &slot.owner_home, false)
                    .await
                {
                    tracing::warn!(
                        owner_uid = slot.owner_uid,
                        error = %error,
                        "persistent App Host lifecycle reconciliation failed"
                    );
                    continue;
                }
            } else {
                continue;
            }
            if !slot_needs_host(&slot).await {
                let runtime = {
                    let mut runtime = slot.runtime.lock().await;
                    runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.last_used.elapsed() >= OWNER_HOST_IDLE)
                        .then(|| runtime.take())
                        .flatten()
                };
                if let Some(runtime) = runtime {
                    if let Err(error) = shutdown_runtime(runtime).await {
                        tracing::error!(
                            owner_uid = slot.owner_uid,
                            error = %error,
                            "idle persistent App Host cleanup failed"
                        );
                    }
                }
            }
        }
    }

    pub(crate) async fn shutdown_all(&self) {
        let runtimes = {
            let mut hosts = self.hosts.lock().await;
            let slots = hosts.drain().map(|(_, slot)| slot).collect::<Vec<_>>();
            drop(hosts);
            let mut runtimes = Vec::new();
            for slot in slots {
                if let Some(runtime) = slot.runtime.lock().await.take() {
                    runtimes.push(runtime);
                }
            }
            runtimes
        };
        for runtime in runtimes {
            if let Err(error) = shutdown_runtime(runtime).await {
                tracing::error!(error = %error, "persistent App Host cleanup failed");
            }
        }
    }
}

async fn require_host_restart_ready(slot: &OwnerHostSlot) -> Result<(), String> {
    let restart = slot.restart.lock().await;
    if let Some(retry_at) = restart
        .retry_at
        .filter(|retry_at| *retry_at > Instant::now())
    {
        return Err(format!(
            "persistent App Host restart is delayed for {}ms after {} failure(s)",
            retry_at
                .saturating_duration_since(Instant::now())
                .as_millis(),
            restart.failures
        ));
    }
    Ok(())
}

async fn record_host_failure(slot: &OwnerHostSlot) {
    let mut restart = slot.restart.lock().await;
    restart.failures = restart.failures.saturating_add(1);
    let delay = Duration::from_secs(1u64 << restart.failures.min(6)).min(APP_RESTART_MAX);
    restart.retry_at = Some(Instant::now() + delay);
}

async fn clear_host_failures(slot: &OwnerHostSlot) {
    let mut restart = slot.restart.lock().await;
    restart.failures = 0;
    restart.retry_at = None;
}

async fn track_service(
    slot: &OwnerHostSlot,
    app_id: &str,
    tool: &str,
    lifecycle: crate::caps::manifest::McpLifecycle,
) -> Arc<Mutex<()>> {
    let mut services = slot.services.lock().await;
    let state = services
        .entry(app_id.to_string())
        .or_insert_with(|| AppServiceState {
            lifecycle,
            tool: tool.to_string(),
            last_used: Instant::now(),
            running: false,
            failures: 0,
            retry_at: None,
            call_lock: Arc::new(Mutex::new(())),
        });
    state.lifecycle = lifecycle;
    state.tool = tool.to_string();
    state.last_used = Instant::now();
    state.call_lock.clone()
}

async fn record_service_result(slot: &OwnerHostSlot, app_id: &str, success: bool) {
    let mut services = slot.services.lock().await;
    let Some(state) = services.get_mut(app_id) else {
        return;
    };
    state.last_used = Instant::now();
    if success {
        state.running = true;
        state.failures = 0;
        state.retry_at = None;
    } else {
        record_service_failure(state);
    }
}

async fn update_reconcile_result(slot: &OwnerHostSlot, app_id: &str, result: Result<bool, String>) {
    let mut services = slot.services.lock().await;
    let Some(state) = services.get_mut(app_id) else {
        return;
    };
    match result {
        Ok(running) => {
            state.running = running;
            state.failures = 0;
            state.retry_at = None;
        }
        Err(error) => {
            record_service_failure(state);
            tracing::warn!(
                owner_uid = slot.owner_uid,
                app_id,
                error = %error,
                "persistent App service lifecycle action failed"
            );
        }
    }
}

fn record_service_failure(state: &mut AppServiceState) {
    state.running = false;
    state.failures = state.failures.saturating_add(1);
    let delay = Duration::from_secs(1u64 << state.failures.min(6)).min(APP_RESTART_MAX);
    state.retry_at = Some(Instant::now() + delay);
}

async fn merge_declared_services(slot: &OwnerHostSlot) {
    let declared = declared_services();
    let declared_ids = declared
        .iter()
        .map(|(app_id, _, _)| app_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut services = slot.services.lock().await;
    for (app_id, state) in services.iter_mut() {
        if !declared_ids.contains(app_id.as_str()) {
            state.lifecycle = crate::caps::manifest::McpLifecycle::Lazy;
            state.last_used = Instant::now()
                .checked_sub(LAZY_APP_IDLE)
                .unwrap_or_else(Instant::now);
        }
    }
    for (app_id, lifecycle, tool) in declared {
        let state = services.entry(app_id).or_insert_with(|| AppServiceState {
            lifecycle,
            tool: tool.clone(),
            last_used: Instant::now(),
            running: false,
            failures: 0,
            retry_at: None,
            call_lock: Arc::new(Mutex::new(())),
        });
        state.lifecycle = lifecycle;
        state.tool = tool;
    }
}

fn declared_services() -> Vec<(String, crate::caps::manifest::McpLifecycle, String)> {
    crate::apps::discover(&apps_root())
        .into_values()
        .filter_map(|app| {
            let service = app.manifest.mcp?;
            let tool = service.tools.first()?.name.clone();
            Some((app.manifest.id, service.lifecycle, tool))
        })
        .collect()
}

fn apps_root() -> PathBuf {
    std::env::var_os("COS_APPS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/apps"))
}

async fn service_snapshots(slot: &OwnerHostSlot) -> Vec<AppServiceSnapshot> {
    slot.services
        .lock()
        .await
        .iter()
        .map(|(app_id, state)| AppServiceSnapshot {
            app_id: app_id.clone(),
            lifecycle: state.lifecycle,
            tool: state.tool.clone(),
            last_used: state.last_used,
            running: state.running,
            retry_at: state.retry_at,
            call_lock: state.call_lock.clone(),
        })
        .collect()
}

async fn service_snapshot(slot: &OwnerHostSlot, app_id: &str) -> Option<AppServiceSnapshot> {
    slot.services
        .lock()
        .await
        .get(app_id)
        .map(|state| AppServiceSnapshot {
            app_id: app_id.to_string(),
            lifecycle: state.lifecycle,
            tool: state.tool.clone(),
            last_used: state.last_used,
            running: state.running,
            retry_at: state.retry_at,
            call_lock: state.call_lock.clone(),
        })
}

fn service_should_run(service: &AppServiceSnapshot, gui_running: bool) -> bool {
    match service.lifecycle {
        crate::caps::manifest::McpLifecycle::Lazy => {
            service.running && service.last_used.elapsed() < LAZY_APP_IDLE
        }
        crate::caps::manifest::McpLifecycle::AlwaysOn => true,
        crate::caps::manifest::McpLifecycle::WhileAppRunning => gui_running,
    }
}

async fn slot_needs_host(slot: &OwnerHostSlot) -> bool {
    let services = service_snapshots(slot).await;
    for service in services {
        match service.lifecycle {
            crate::caps::manifest::McpLifecycle::AlwaysOn => return true,
            crate::caps::manifest::McpLifecycle::WhileAppRunning => {
                if gui_app_is_running(slot.owner_uid, &slot.owner_home, &service.app_id, None).await
                {
                    return true;
                }
            }
            crate::caps::manifest::McpLifecycle::Lazy if service.running => return true,
            crate::caps::manifest::McpLifecycle::Lazy => {}
        }
    }
    false
}

async fn gui_app_is_running(
    owner_uid: u32,
    owner_home: &std::path::Path,
    app_id: &str,
    host_session_id: Option<&str>,
) -> bool {
    let sessions = crate::paths::with_user_override(owner_uid, owner_home.to_path_buf(), async {
        crate::proc::registry_sessions()
    })
    .await;
    sessions.into_iter().any(|session| {
        session.group.as_deref() == Some("app")
            && session.app_id.as_deref() == Some(app_id)
            && host_session_id
                .is_none_or(|host_session_id| session.parent.as_deref() != Some(host_session_id))
            && !session.pending_bind
            && session.pid > 1
            && session.start_time_ticks.is_some()
            && crate::proc::read_start_time_ticks_pub(session.pid) == session.start_time_ticks
    })
}

fn persistent_approved_paths(
    identity: &WorkerIdentity,
) -> Result<Vec<crate::extension_host::protocol::ApprovedPath>, String> {
    let mut candidates = vec![identity.home.clone()];
    for path in ["/usr/lib/cos/apps", "/usr/lib/cos/python"] {
        let path = PathBuf::from(path);
        if path.exists() {
            candidates.push(path);
        }
    }
    for key in ["COS_APPS_DIR", "COS_SDK_PYTHON_DIR"] {
        if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
            if path.exists() {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    if candidates.len() > 64 {
        return Err("persistent App Host has more than 64 approved paths".to_string());
    }
    candidates
        .iter()
        .map(|path| crate::extension_host::spawn::approve_runtime_path(path, identity.uid))
        .collect()
}

fn drain_host_output(host: &mut SpawnedExtensionHost, owner_uid: u32) {
    if let Some(stdout) = host.child.stdout.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(owner_uid, output = %line, "persistent App Host output");
            }
        });
    }
    if let Some(stderr) = host.child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(owner_uid, output = %line, "persistent App Host error");
            }
        });
    }
}

fn record_call(
    authority: &super::app_sessions::AgentGatewayAuthority,
    binding: &crate::extension_host::protocol::ExtensionBinding,
    invocation: AppInvocationAudit,
    latency: Duration,
    error: Option<&str>,
) {
    let binding_digest = match binding.digest() {
        Ok(digest) => digest,
        Err(error) => {
            tracing::error!(error = %error, "could not encode persistent Host audit binding");
            return;
        }
    };
    let record = crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
        session_id: authority.session_id.clone(),
        kind: ExtensionKind::App,
        action: LifecycleAction::Call,
        extension_id: invocation.app_id.clone(),
        binding_digest,
        lease_digest: crate::crypto::sha256_hex(binding.lease_nonce.as_bytes()),
        stage: Some(AuditStage::Gateway),
        mcp: None,
        app: Some(Box::new(invocation)),
        manifest_digest: None,
        success: error.is_none(),
        latency_ms: latency.as_millis() as u64,
        error: error.map(crate::audit_policy::text_digest),
    };
    crate::clawd::audit::record_worker_runtime(&authority.task_id, authority.owner_uid, &record);
}

async fn shutdown_runtime(mut runtime: PersistentHostRuntime) -> Result<(), String> {
    let _ = runtime.client.shutdown().await;
    runtime.lease.close();
    runtime.broker_task.abort();
    let cleanup = cleanup_started_host(
        &mut runtime.host,
        runtime.owner_uid,
        runtime.extension_uid,
        Some(&runtime.host_session_id),
    )
    .await;
    let mut cleanup = cleanup;
    if cleanup.is_ok() {
        if let Some(identity) = runtime.identity.take() {
            cleanup = identity.release();
        }
    }
    crate::clawd::audit::record_extension_host_event(
        &format!("owner-host-{}", runtime.owner_uid),
        None,
        runtime.owner_uid,
        runtime.host.pid,
        runtime.host.start_time_ticks,
        if cleanup.is_ok() {
            "detach"
        } else {
            "cleanup-failed"
        },
        cleanup.is_ok(),
    );
    cleanup
}

async fn cleanup_started_host(
    host: &mut SpawnedExtensionHost,
    owner_uid: u32,
    extension_uid: u32,
    host_session_id: Option<&str>,
) -> Result<(), String> {
    if let Some(session_id) = host_session_id {
        for child in crate::proc::deregister_child_sessions_for_owner(session_id, owner_uid) {
            crate::clawd::authority::revoke_session_for_owner(&child, owner_uid);
        }
        crate::proc::deregister_session_for_owner(session_id, owner_uid);
        crate::clawd::authority::revoke_session_for_owner(session_id, owner_uid);
    }
    let _ = host.child.start_kill();
    let containment = host.cgroup.cleanup().await;
    let reaped = match tokio::time::timeout(HOST_SHUTDOWN_GRACE, host.child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(format!("reap persistent App Host {}: {error}", host.pid)),
        Err(_) => Err(format!(
            "persistent App Host {} did not exit before cleanup",
            host.pid
        )),
    };
    let mounts = if containment.is_ok() {
        host.cleanup_private_mounts()
    } else {
        Err("private mounts retained because containment cleanup failed".to_string())
    };
    let paths = if containment.is_ok() && mounts.is_ok() {
        host.paths.cleanup()
    } else {
        Err("Host state retained because containment cleanup failed".to_string())
    };
    let acl = if containment.is_ok() && mounts.is_ok() && paths.is_ok() {
        crate::storage::remove_routed_extension_reader(owner_uid, extension_uid)
    } else {
        Err("routed ACL retained because Host cleanup failed".to_string())
    };
    merge_cleanups([containment, reaped, mounts, paths, acl])
}

fn cleanup_start_error(
    error: String,
    cleanup: Result<(), String>,
    identity: ExtensionIdentityLease,
) -> String {
    if cleanup.is_ok() {
        merge_error(error, identity.release())
    } else {
        merge_error(error, cleanup.and_then(|_| identity.release()))
    }
}

fn release_after_error(identity: ExtensionIdentityLease, error: String) -> String {
    merge_error(error, identity.release())
}

fn merge_cleanup(first: Result<(), String>, second: Result<(), String>) -> Result<(), String> {
    merge_cleanups([first, second])
}

fn merge_cleanups<const N: usize>(cleanups: [Result<(), String>; N]) -> Result<(), String> {
    let errors = cleanups
        .into_iter()
        .filter_map(Result::err)
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn merge_error(error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; cleanup failed: {cleanup}"),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_host.rs"
    ));
}

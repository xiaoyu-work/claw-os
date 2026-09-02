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
use crate::caps::Role;
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
    runtime: Mutex<Option<PersistentHostRuntime>>,
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
            ) {
                Ok(authorized) => break authorized,
                Err(error) if error.audit_class == Some("approval_required") => {
                    context.remaining(APPROVAL_POLL)?;
                    tokio::time::sleep(APPROVAL_POLL).await;
                }
                Err(error) => return Err(error.message),
            }
        };
        let (client, binding) = self
            .get_or_start(authority.owner_uid, &authority.owner_home)
            .await?;
        let gateway_handle = super::app_sessions::issue_gateway_dispatch_grant(
            &binding,
            &request.app_id,
            &request.tool,
            &authorized.arguments,
            &context,
            &authority.capability_generation,
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
        result
    }

    async fn get_or_start(
        &self,
        owner_uid: u32,
        owner_home: &std::path::Path,
    ) -> Result<
        (
            Arc<ExtensionHostClient>,
            crate::extension_host::protocol::ExtensionBinding,
        ),
        String,
    > {
        let slot = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(owner_uid)
                .or_insert_with(|| {
                    Arc::new(OwnerHostSlot {
                        runtime: Mutex::new(None),
                    })
                })
                .clone()
        };
        let mut runtime = slot.runtime.lock().await;
        let live = runtime.as_mut().is_some_and(|active| {
            crate::proc::read_start_time_ticks_pub(active.host.pid) == active.host.start_time_ticks
                && active.host.child.try_wait().ok().flatten().is_none()
        });
        let stale = (!live).then(|| runtime.take()).flatten();
        if let Some(stale) = stale {
            if let Err(error) = shutdown_runtime(stale).await {
                return Err(format!("stale persistent App Host cleanup failed: {error}"));
            }
        }
        if runtime.is_none() {
            *runtime = Some(self.start_owner_host(owner_uid, owner_home).await?);
        }
        let active = runtime
            .as_ref()
            .ok_or_else(|| "persistent App Host was not installed".to_string())?;
        Ok((active.client.clone(), active.host.binding.clone()))
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
        let host_caps = super::system_caps::system_agent_caps(owner_uid, owner_home);
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
            u64::MAX,
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
        })
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
    let record = crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
        session_id: authority.session_id.clone(),
        kind: ExtensionKind::App,
        action: LifecycleAction::Call,
        extension_id: invocation.app_id.clone(),
        binding_digest: binding
            .digest()
            .expect("validated persistent Host binding must serialize"),
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

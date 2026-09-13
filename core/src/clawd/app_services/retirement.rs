//! Checked service retirement retains custody across errors and cancelled waits.

use super::{join_cleanup_result, ServiceRuntime, ServiceSlot};

const BROKER_RETIREMENT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) struct StartupGuard<'a>(&'a mut ServiceRuntime);

impl<'a> StartupGuard<'a> {
    pub(super) fn new(runtime: &'a mut ServiceRuntime) -> Self {
        runtime.retiring = true;
        Self(runtime)
    }

    pub(super) fn ready(
        self,
        client: std::sync::Arc<crate::extension_host::client::ExtensionHostClient>,
    ) {
        self.0.client = Some(client);
        self.0.retiring = false;
    }
}

impl Drop for StartupGuard<'_> {
    fn drop(&mut self) {
        if self.0.retiring {
            if let Err(error) = close_admission(self.0) {
                tracing::error!(
                    owner = self.0.lease.owner_uid,
                    app = %self.0.package.id,
                    %error,
                    "interrupted App service startup could not record retirement"
                );
            }
        }
    }
}

impl ServiceSlot {
    pub(super) async fn retire(&mut self) -> Result<(), String> {
        if let Some(runtime) = self.runtime.as_mut() {
            stop_runtime(runtime).await.map_err(|error| {
                format!(
                    "App service `{}` retirement for owner {} remains incomplete: {error}",
                    runtime.package.id, runtime.lease.owner_uid
                )
            })?;
        }
        self.runtime = None;
        Ok(())
    }
}

fn close_admission(runtime: &mut ServiceRuntime) -> Result<(), String> {
    runtime.retiring = true;
    runtime.lease.close();
    runtime.client = None;
    runtime.identity.begin_retirement()
}

async fn stop_runtime(runtime: &mut ServiceRuntime) -> Result<(), String> {
    close_admission(runtime)?;
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
    let broker = if let Some(error) = &runtime.broker_failure {
        Err(error.clone())
    } else if let Some(task) = runtime.broker_task.as_mut() {
        match tokio::time::timeout(BROKER_RETIREMENT_GRACE, task).await {
            Err(_) => Err("App service broker requests are still draining".to_string()),
            Ok(stopped) => {
                runtime.broker_task = None;
                match stopped {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        let error = format!(
                            "App service broker task failed: {error}; broker recovery is required before identity reuse"
                        );
                        runtime.broker_failure = Some(error.clone());
                        Err(error)
                    }
                }
            }
        }
    } else {
        Ok(())
    };
    let containment = crate::agentd::supervisor::reap_extension_host(&mut runtime.host).await;
    join_cleanup_result(broker, containment)?;

    let acl = crate::storage::remove_routed_extension_reader(
        runtime.lease.owner_uid,
        runtime.extension_uid,
    );
    join_cleanup_result(
        acl,
        crate::storage::purge_routed_extension_reader(runtime.extension_uid),
    )?;
    runtime.identity.release_checked()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_services/retirement.rs"
    ));
}

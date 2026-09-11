//! Stateful Apps use the same sandbox provider as one-shot operations.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use tokio::process::Child;

use super::{kill_and_reap_child, pathsep, safe_session_env_allowlist, Runtime, SessionBinding};

pub(super) struct SessionProcess {
    child: Option<Child>,
    resources: crate::worker::LaunchResources,
}

impl SessionProcess {
    pub(super) fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("session child is owned until drop")
    }

    pub(super) fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    pub(super) fn terminate(&mut self) {
        self.resources.kill_all(self.id());
        if let Some(child) = self.child.take() {
            kill_and_reap_child(child);
        }
    }
}

impl Drop for SessionProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(super) fn spawn(
    launch: &crate::bridge::AppLaunch,
    bound: &SessionBinding,
    session: &crate::bridge::AppIdentitySession,
    apps: &Path,
    data: &str,
    python: &[std::path::PathBuf],
) -> Result<SessionProcess, String> {
    let entry = bound.entry_path();
    let (program, argv) = match launch.manifest().runtime {
        Runtime::Python => (
            crate::bridge::interpreter_path("python3")?,
            vec![entry.to_string_lossy().into_owned()],
        ),
        Runtime::Node => (
            crate::bridge::interpreter_path("node")?,
            vec![entry.to_string_lossy().into_owned()],
        ),
        Runtime::Shell => (
            crate::bridge::interpreter_path("bash")?,
            vec![entry.to_string_lossy().into_owned()],
        ),
        Runtime::Binary => (entry.to_path_buf(), Vec::new()),
    };
    let mut extra_env: BTreeMap<String, String> =
        safe_session_env_allowlist().into_iter().collect();
    let mut pythonpath: Vec<_> = python
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    pythonpath.push(apps.to_string_lossy().into_owned());
    extra_env.insert("PYTHONPATH".into(), pythonpath.join(pathsep()));
    extra_env.insert("COS_MCP_SERVER".into(), "1".into());
    // Persistent servers receive no standing host-file or network mounts.
    // Call-scoped resources are exercised through the live broker authority.
    let empty = crate::caps::CapSet::new();
    let mut policy =
        crate::worker::derive::app_operation(crate::worker::derive::AppOperationInput {
            app_id: launch.app_id(),
            app_dir: launch.dir(),
            operation: "session",
            program,
            argv,
            caps: &empty,
            session_id: session.id(),
            data_dir: data,
            apps_dir: apps
                .to_str()
                .ok_or_else(|| "App root is not UTF-8".to_string())?,
            extra_env,
            stdio: crate::worker::StdioPlan::Streamed,
            desktop: false,
            package_identity: bound.package_identity,
            pinned_entries: bound.pinned_entries.clone(),
            developer: bound.binding.is_developer(),
        })?;
    policy.limits = crate::worker::Limits::server();
    for directory in python {
        let canonical = directory
            .canonicalize()
            .map_err(|error| format!("resolve App Python runtime: {error}"))?;
        if !policy.mounts.iter().any(|mount| mount.target == canonical) {
            policy.mounts.push(crate::worker::Mount::read_only(
                canonical.clone(),
                canonical,
                crate::worker::MountClass::Runtime,
            ));
        }
    }
    bound.assert_pinned()?;
    let worker = crate::worker::WorkerLaunch::new(policy).with_authority(
        session
            .broker_authority(launch.app_id())
            .with_package(bound.binding.package_ref()),
    );
    let prepared = crate::worker::prepare(&worker).inspect_err(|error| {
        crate::worker::audit::refused(
            &format!("app:{}/session", launch.app_id()),
            crate::worker::TrustTier::AppOperation.as_str(),
            error,
        );
    })?;
    crate::worker::audit::launched(&prepared.facts, Some(session.id()));
    let crate::worker::PreparedLaunch {
        command, resources, ..
    } = prepared;
    let mut command = tokio::process::Command::from(command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("spawn sandboxed App session: {error}"))?;
    Ok(SessionProcess {
        child: Some(child),
        resources,
    })
}

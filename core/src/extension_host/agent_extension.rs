//! Generic host-side lifecycle for one verified Agent extension process.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::agent_extensions::capability_ref::CapabilityReference;
use crate::agent_extensions::manifest::{ExtensionManifest, ABI_VERSION};

use super::abi::{
    AbiBinding, AbiRequest, AbiResponse, EventPayload, HostMessage, ShutdownReason,
    INITIALIZE_TIMEOUT, SHUTDOWN_TIMEOUT,
};
use super::protocol::{AgentExtensionRegistration, AgentExtensionResult, ExtensionBinding};

pub(crate) struct HostedAgentExtension {
    manifest: ExtensionManifest,
    binding: AbiBinding,
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    sequence: u64,
    materialized_root: PathBuf,
    known_descendants: BTreeMap<u32, u64>,
}

struct ExtensionLaunchGuard {
    materialized_root: Option<PathBuf>,
    child: Option<Child>,
}

impl ExtensionLaunchGuard {
    fn new(materialized_root: PathBuf) -> Self {
        Self {
            materialized_root: Some(materialized_root),
            child: None,
        }
    }

    fn root(&self) -> &Path {
        self.materialized_root
            .as_deref()
            .expect("launch guard owns materialized root")
    }

    async fn fail<T>(&mut self, error: String) -> Result<T, String> {
        let mut cleanup_errors = Vec::new();
        if let Some(child) = self.child.as_mut() {
            let descendants = child.id().map(capture_descendants).unwrap_or_default();
            for descendant in &descendants {
                unsafe {
                    libc::kill(descendant.pid as libc::pid_t, libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            if tokio::time::timeout(Duration::from_secs(1), child.wait())
                .await
                .is_err()
            {
                cleanup_errors.push("unattached extension child did not exit".to_string());
            }
            reap_captured_descendants(descendants).await;
        }
        self.child = None;
        if let Some(root) = self.materialized_root.take() {
            if let Err(cleanup) = std::fs::remove_dir_all(&root) {
                if cleanup.kind() != std::io::ErrorKind::NotFound {
                    cleanup_errors
                        .push(format!("remove unattached materialized package: {cleanup}"));
                }
            }
        }
        if cleanup_errors.is_empty() {
            Err(error)
        } else {
            Err(format!("{error}; {}", cleanup_errors.join("; ")))
        }
    }

    fn disarm(&mut self) -> (PathBuf, Child) {
        (
            self.materialized_root
                .take()
                .expect("launch guard owns materialized root"),
            self.child.take().expect("launch guard owns child"),
        )
    }
}

impl Drop for ExtensionLaunchGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
        if let Some(root) = self.materialized_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

impl HostedAgentExtension {
    pub async fn attach(
        registration: &AgentExtensionRegistration,
        host_binding: &ExtensionBinding,
        isolation: &super::child_isolation::IsolationAuthority,
    ) -> Result<Self, String> {
        registration.validate()?;
        let package_path =
            crate::agent_extensions::registry::installed_root().join(&registration.extension_id);
        let receipt = host_binding
            .agent_extensions
            .iter()
            .find(|receipt| {
                receipt.id == registration.extension_id
                    && receipt.version == registration.extension_version
                    && receipt.content_digest == registration.package_digest
            })
            .ok_or_else(|| {
                "extension registration was not authenticated by the owner-qualified broker"
                    .to_string()
            })?;
        let mut options =
            crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::AgentExtension)
                .expect_id(&registration.extension_id);
        options.allow_developer = false;
        let package = crate::provenance::verify::verify_package_with_receipt(
            &package_path,
            &options,
            receipt,
        )
        .map_err(|error| {
            crate::provenance::quarantine_reason(
                crate::provenance::PackageKind::AgentExtension,
                &registration.extension_id,
                &error,
            )
        })?;
        let manifest = ExtensionManifest::parse_verified(&package)?;
        let manifest_digest = ExtensionManifest::manifest_digest(&package)?;
        if registration.extension_id != manifest.identity.id
            || registration.extension_version != manifest.identity.version
            || registration.package_digest != package.content_digest()
            || registration.manifest_digest != manifest_digest
            || registration.content_digest != manifest.identity.content_digest
        {
            return Err("extension registration does not match the verified manifest".to_string());
        }
        let session_id = host_binding
            .session_id
            .clone()
            .ok_or_else(|| "Agent extensions require a durable session".to_string())?;
        package
            .assert_tree_current()
            .map_err(|error| format!("extension package changed before launch: {error}"))?;
        let entry_bytes = package
            .read_verified(&manifest.entry)
            .map_err(|error| format!("read verified extension entry: {error}"))?;
        let materialized_root = materialize(&package)?;
        let mut launch_guard = ExtensionLaunchGuard::new(materialized_root);
        package
            .assert_tree_current()
            .map_err(|error| format!("extension package changed during launch: {error}"))?;
        let entry = launch_guard.root().join(&manifest.entry);
        let binding = AbiBinding {
            task_id: host_binding.task_id.clone(),
            session_id,
            owner_uid: host_binding.owner_uid,
            extension_id: manifest.identity.id.clone(),
            extension_version: manifest.identity.version.clone(),
            package_digest: package.content_digest().to_string(),
            manifest_digest,
            entry_digest: crate::crypto::sha256_hex(&entry_bytes),
            capability_generation: host_binding.capability_generation.clone(),
            lease_digest: crate::crypto::sha256_hex(host_binding.lease_nonce.as_bytes()),
            instance_nonce: random_nonce()?,
            additive: BTreeMap::new(),
        };
        binding.validate()?;

        let launch = super::child_isolation::prepare_verified_package(
            &entry,
            launch_guard.root(),
            vec![(
                OsString::from("CLAW_EXTENSION_ABI"),
                OsString::from(ABI_VERSION.to_string()),
            )],
            isolation,
        )?;
        let preexisting_children = direct_children(std::process::id());
        let mut command = tokio::process::Command::new(&launch.program);
        command
            .args(&launch.args)
            .env_clear()
            .envs(launch.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        super::child_isolation::close_unallowlisted_fds(command.as_std_mut());
        launch_guard.child = Some(
            command
                .spawn()
                .map_err(|error| format!("spawn Agent extension: {error}"))?,
        );
        let stdin = match launch_guard
            .child
            .as_mut()
            .and_then(|child| child.stdin.take())
        {
            Some(stdin) => stdin,
            None => {
                return launch_guard
                    .fail("Agent extension stdin was not captured".to_string())
                    .await;
            }
        };
        let stdout = match launch_guard
            .child
            .as_mut()
            .and_then(|child| child.stdout.take())
        {
            Some(stdout) => stdout,
            None => {
                return launch_guard
                    .fail("Agent extension stdout was not captured".to_string())
                    .await;
            }
        };
        let (materialized_root, child) = launch_guard.disarm();
        let mut hosted = Self {
            manifest,
            binding,
            child,
            stdin,
            stdout,
            sequence: 0,
            materialized_root,
            known_descendants: BTreeMap::new(),
        };
        let initialize = AbiRequest {
            protocol: ABI_VERSION,
            binding: hosted.binding.clone(),
            sequence: 0,
            message: HostMessage::Initialize {
                min_version: hosted.manifest.protocol.min_version,
                max_version: hosted.manifest.protocol.max_version,
                required_features: hosted.manifest.protocol.required_features.clone(),
                subscriptions: hosted.manifest.subscriptions.clone(),
                requested_capability_count: hosted.manifest.requested_capabilities.len(),
            },
            additive: BTreeMap::new(),
        };
        let response = match hosted.exchange(&initialize, INITIALIZE_TIMEOUT).await {
            Ok(response) => response,
            Err(error) => {
                return hosted
                    .fail_closed(format!("initialize Agent extension: {error}"))
                    .await;
            }
        };
        if let Err(error) = super::abi::validate_ready(
            &initialize,
            &response,
            hosted.manifest.protocol.min_version,
            hosted.manifest.protocol.max_version,
            &hosted.manifest.protocol.required_features,
        ) {
            return hosted.fail_closed(error).await;
        }
        hosted.sequence = 1;
        hosted.remember_new_host_children(&preexisting_children);
        Ok(hosted)
    }

    pub fn binding(&self) -> &AbiBinding {
        &self.binding
    }

    pub async fn event(
        &mut self,
        binding: &AbiBinding,
        event_id: String,
        deadline: super::abi::MonotonicDeadlineNs,
        payload: EventPayload,
        capability_refs: Vec<CapabilityReference>,
    ) -> Result<AgentExtensionResult, String> {
        if binding != &self.binding {
            return self
                .fail_closed(
                    "extension event binding does not match the active instance".to_string(),
                )
                .await;
        }
        if let Err(error) = payload.validate() {
            return self.fail_closed(error).await;
        }
        if !self.manifest.subscriptions.contains(&payload.kind()) {
            return self
                .fail_closed("extension event was not declared in the manifest".to_string())
                .await;
        }
        if capability_refs.len() != self.manifest.action_policies.len() {
            return self
                .fail_closed("extension event capability references are incomplete".to_string())
                .await;
        }
        let timeout = match deadline.remaining() {
            Ok(timeout) => timeout,
            Err(error) => return self.fail_closed(error).await,
        };
        if timeout > Duration::from_millis(self.manifest.limits.event_timeout_ms) {
            return self
                .fail_closed("extension event deadline exceeds the manifest limit".to_string())
                .await;
        }
        let request = AbiRequest {
            protocol: ABI_VERSION,
            binding: self.binding.clone(),
            sequence: self.sequence,
            message: HostMessage::Event {
                event_id: event_id.clone(),
                deadline_monotonic_ns: deadline,
                payload,
                capability_refs,
            },
            additive: BTreeMap::new(),
        };
        let response = match self.exchange(&request, timeout).await {
            Ok(response) => response,
            Err(error) => return self.fail_closed(error).await,
        };
        let (output, proposed_actions) = match super::abi::validate_result(
            &request,
            &response,
            &event_id,
            self.manifest.limits.max_output_bytes,
            self.manifest.limits.max_actions_per_event,
        ) {
            Ok(result) => result,
            Err(error) => return self.fail_closed(error).await,
        };
        self.sequence = self.sequence.saturating_add(1);
        Ok(AgentExtensionResult {
            output: output.clone(),
            proposed_actions: proposed_actions.to_vec(),
        })
    }

    pub async fn shutdown(&mut self, reason: ShutdownReason) -> Result<(), String> {
        let request = AbiRequest {
            protocol: ABI_VERSION,
            binding: self.binding.clone(),
            sequence: self.sequence,
            message: HostMessage::Shutdown { reason },
            additive: BTreeMap::new(),
        };
        let outcome = match self.exchange(&request, SHUTDOWN_TIMEOUT).await {
            Ok(response) => super::abi::validate_shutdown(&request, &response),
            Err(error) => Err(error),
        };
        let cleanup = self.cleanup().await;
        match (outcome, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
        }
    }

    pub async fn abort(&mut self) {
        let _ = self.cleanup().await;
    }

    async fn fail_closed<T>(&mut self, error: String) -> Result<T, String> {
        match self.cleanup().await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; {cleanup}")),
        }
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        let process = self.kill_and_wait().await;
        let storage = match std::fs::remove_dir_all(&self.materialized_root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove materialized extension package: {error}")),
        };
        match (process, storage) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(storage)) => Err(format!("{error}; {storage}")),
        }
    }

    async fn exchange(
        &mut self,
        request: &AbiRequest,
        timeout: Duration,
    ) -> Result<AbiResponse, String> {
        self.remember_descendants();
        let result = tokio::time::timeout(timeout, async {
            super::abi::write_request(&mut self.stdin, request).await?;
            super::abi::read_response(&mut self.stdout).await
        })
        .await
        .map_err(|_| format!("Agent extension timed out after {}ms", timeout.as_millis()))?;
        self.remember_descendants();
        result
    }

    async fn kill_and_wait(&mut self) -> Result<(), String> {
        self.remember_descendants();
        let descendants: Vec<ProcessIdentity> = self
            .known_descendants
            .iter()
            .map(|(pid, start_time_ticks)| ProcessIdentity {
                pid: *pid,
                start_time_ticks: *start_time_ticks,
            })
            .collect();
        for descendant in &descendants {
            if process_parent_and_start(descendant.pid)
                .is_some_and(|(_, start)| start == descendant.start_time_ticks)
            {
                unsafe {
                    libc::kill(descendant.pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        if tokio::time::timeout(Duration::from_millis(100), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await;
        }
        reap_captured_descendants(descendants).await;
        let survivors = self
            .known_descendants
            .iter()
            .filter_map(|(pid, expected_start)| {
                process_parent_and_start(*pid)
                    .filter(|(_, actual_start)| actual_start == expected_start)
                    .map(|_| *pid)
            })
            .collect::<Vec<_>>();
        if survivors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Agent extension descendant cleanup left live processes {survivors:?}"
            ))
        }
    }

    fn remember_descendants(&mut self) {
        let mut roots = self.known_descendants.keys().copied().collect::<Vec<_>>();
        if let Some(pid) = self.child.id() {
            roots.push(pid);
        }
        for root in roots {
            for descendant in capture_descendants(root) {
                self.known_descendants
                    .insert(descendant.pid, descendant.start_time_ticks);
            }
        }
    }

    fn remember_new_host_children(&mut self, preexisting: &[ProcessIdentity]) {
        for child in direct_children(std::process::id()) {
            if !preexisting.iter().any(|existing| {
                existing.pid == child.pid && existing.start_time_ticks == child.start_time_ticks
            }) && process_mentions_root(child.pid, &self.materialized_root)
            {
                self.known_descendants
                    .insert(child.pid, child.start_time_ticks);
            }
        }
    }
}

impl Drop for HostedAgentExtension {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.materialized_root);
    }
}

fn materialize(package: &crate::provenance::VerifiedPackage) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "extension host HOME is unavailable".to_string())?;
    let parent = home.join("verified-packages");
    std::fs::create_dir_all(&parent)
        .map_err(|error| format!("create verified package storage: {error}"))?;
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect verified package storage: {error}"))?;
    let root = parent.join(format!(
        "{}-{}-{}",
        package.id(),
        package.short_digest(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&root)
        .map_err(|error| format!("create verified package instance: {error}"))?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect verified package instance: {error}"))?;
    for signed in package.files() {
        let destination = root.join(&signed.path);
        if signed.kind == crate::provenance::envelope::NodeKind::Dir {
            std::fs::create_dir_all(&destination)
                .map_err(|error| format!("create verified package directory: {error}"))?;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("protect verified package directory: {error}"))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create verified package directory: {error}"))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("protect verified package directory: {error}"))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&destination)
            .map_err(|error| format!("create verified package file: {error}"))?;
        use std::io::Write;
        let bytes = package
            .read_verified(&signed.path)
            .map_err(|error| format!("read verified package file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("write verified package file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync verified package file: {error}"))?;
        std::fs::set_permissions(
            &destination,
            std::fs::Permissions::from_mode(if signed.mode & 0o111 != 0 {
                0o500
            } else {
                0o400
            }),
        )
        .map_err(|error| format!("protect verified package file: {error}"))?;
    }
    Ok(root)
}

fn random_nonce() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    crate::credential::os_random_bytes(&mut bytes)
        .map_err(|error| format!("generate extension instance nonce: {error}"))?;
    Ok(hex::encode(bytes))
}

#[derive(Clone, Copy)]
struct ProcessIdentity {
    pid: u32,
    start_time_ticks: u64,
}

fn capture_descendants(root: u32) -> Vec<ProcessIdentity> {
    let mut parents = BTreeMap::<u32, Vec<ProcessIdentity>>::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Some((parent, start_time_ticks)) = process_parent_and_start(pid) else {
            continue;
        };
        parents.entry(parent).or_default().push(ProcessIdentity {
            pid,
            start_time_ticks,
        });
    }

    let mut descendants = Vec::new();
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        if let Some(children) = parents.get(&parent) {
            descendants.extend(children.iter().copied());
            pending.extend(children.iter().map(|child| child.pid));
        }
    }
    descendants
}

fn direct_children(parent: u32) -> Vec<ProcessIdentity> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let (actual_parent, start_time_ticks) = process_parent_and_start(pid)?;
            (actual_parent == parent).then_some(ProcessIdentity {
                pid,
                start_time_ticks,
            })
        })
        .collect()
}

fn process_parent_and_start(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .collect::<Vec<_>>();
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

fn process_mentions_root(pid: u32, root: &Path) -> bool {
    let Ok(command) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let needle = root.as_os_str().as_bytes();
    command
        .windows(needle.len())
        .any(|candidate| candidate == needle)
}

async fn reap_captured_descendants(descendants: Vec<ProcessIdentity>) {
    let host_pid = std::process::id();
    for _ in 0..100 {
        let mut live = false;
        for descendant in descendants.iter().rev() {
            let Some((parent, start_time)) = process_parent_and_start(descendant.pid) else {
                continue;
            };
            if start_time != descendant.start_time_ticks {
                continue;
            }
            live = true;
            unsafe {
                libc::kill(descendant.pid as libc::pid_t, libc::SIGKILL);
            }
            if parent == host_pid {
                unsafe {
                    libc::waitpid(
                        descendant.pid as libc::pid_t,
                        std::ptr::null_mut(),
                        libc::WNOHANG,
                    );
                }
            }
        }
        if !live {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/agent_extension.rs"
    ));
}

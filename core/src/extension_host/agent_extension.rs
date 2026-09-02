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

impl HostedAgentExtension {
    pub async fn attach(
        snapshot: crate::provenance::PackageSnapshot,
        registration: &AgentExtensionRegistration,
        host_binding: &ExtensionBinding,
        isolation: &super::child_isolation::IsolationAuthority,
    ) -> Result<Self, String> {
        let package = crate::provenance::verify_snapshot(
            &snapshot,
            crate::provenance::PackageKind::AgentExtension,
        )?;
        let manifest = ExtensionManifest::parse_verified(&package)?;
        registration.validate()?;
        let manifest_digest = ExtensionManifest::manifest_digest(&package)?;
        if registration.extension_id != manifest.identity.id
            || registration.extension_version != manifest.identity.version
            || registration.package_digest != package.digest()
            || registration.manifest_digest != manifest_digest
            || registration.content_digest != manifest.identity.content_digest
        {
            return Err("extension registration does not match the verified manifest".to_string());
        }
        let session_id = host_binding
            .session_id
            .clone()
            .ok_or_else(|| "Agent extensions require a durable session".to_string())?;
        let entry_bytes = package
            .file_bytes(&manifest.entry)
            .ok_or_else(|| "verified extension entry disappeared".to_string())?;
        let materialized_root = materialize(&package)?;
        let entry = materialized_root.join(&manifest.entry);
        let binding = AbiBinding {
            task_id: host_binding.task_id.clone(),
            session_id,
            owner_uid: host_binding.owner_uid,
            extension_id: manifest.identity.id.clone(),
            extension_version: manifest.identity.version.clone(),
            package_digest: package.digest().to_string(),
            manifest_digest,
            entry_digest: crate::crypto::sha256_hex(entry_bytes),
            capability_generation: host_binding.capability_generation.clone(),
            lease_digest: crate::crypto::sha256_hex(host_binding.lease_nonce.as_bytes()),
            instance_nonce: random_nonce()?,
            additive: BTreeMap::new(),
        };
        binding.validate()?;

        let launch = super::child_isolation::prepare_verified_package(
            &entry,
            &materialized_root,
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
        let mut child = command
            .spawn()
            .map_err(|error| format!("spawn Agent extension: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Agent extension stdin was not captured".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Agent extension stdout was not captured".to_string())?;
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
        let response = hosted
            .exchange(&initialize, INITIALIZE_TIMEOUT)
            .await
            .map_err(|error| format!("initialize Agent extension: {error}"))?;
        if let Err(error) = super::abi::validate_ready(
            &initialize,
            &response,
            hosted.manifest.protocol.min_version,
            hosted.manifest.protocol.max_version,
            &hosted.manifest.protocol.required_features,
        ) {
            let _ = hosted.kill_and_wait().await;
            return Err(error);
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
        payload: EventPayload,
        capability_refs: Vec<CapabilityReference>,
    ) -> Result<AgentExtensionResult, String> {
        if binding != &self.binding {
            let cleanup = self.kill_and_wait().await;
            if let Err(cleanup) = cleanup {
                return Err(format!(
                    "extension event binding does not match the active instance; {cleanup}"
                ));
            }
            return Err("extension event binding does not match the active instance".to_string());
        }
        if let Err(error) = payload.validate() {
            return match self.kill_and_wait().await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; {cleanup}")),
            };
        }
        if !self.manifest.subscriptions.contains(&payload.kind()) {
            let error = "extension event was not declared in the manifest".to_string();
            return match self.kill_and_wait().await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; {cleanup}")),
            };
        }
        if capability_refs.len() != self.manifest.requested_capabilities.len() {
            let error = "extension event capability references are incomplete".to_string();
            return match self.kill_and_wait().await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; {cleanup}")),
            };
        }
        let timeout = Duration::from_millis(self.manifest.limits.event_timeout_ms);
        let request = AbiRequest {
            protocol: ABI_VERSION,
            binding: self.binding.clone(),
            sequence: self.sequence,
            message: HostMessage::Event {
                event_id: event_id.clone(),
                deadline_ms: now_ms().saturating_add(self.manifest.limits.event_timeout_ms),
                payload,
                capability_refs,
            },
            additive: BTreeMap::new(),
        };
        let response = match self.exchange(&request, timeout).await {
            Ok(response) => response,
            Err(error) => {
                return match self.kill_and_wait().await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!("{error}; {cleanup}")),
                };
            }
        };
        let (output, proposed_actions) = match super::abi::validate_result(
            &request,
            &response,
            &event_id,
            self.manifest.limits.max_output_bytes,
            self.manifest.limits.max_actions_per_event,
        ) {
            Ok(result) => result,
            Err(error) => {
                return match self.kill_and_wait().await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!("{error}; {cleanup}")),
                };
            }
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
        self.kill_and_wait().await?;
        let _ = std::fs::remove_dir_all(&self.materialized_root);
        outcome
    }

    pub async fn abort(&mut self) {
        let _ = self.kill_and_wait().await;
        let _ = std::fs::remove_dir_all(&self.materialized_root);
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
        &package.digest()[..16],
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&root)
        .map_err(|error| format!("create verified package instance: {error}"))?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect verified package instance: {error}"))?;
    for signed in package.signed_files() {
        let destination = root.join(&signed.path);
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
        file.write_all(
            package
                .file_bytes(&signed.path)
                .ok_or_else(|| "verified package inventory drifted".to_string())?,
        )
        .map_err(|error| format!("write verified package file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync verified package file: {error}"))?;
        std::fs::set_permissions(
            &destination,
            std::fs::Permissions::from_mode(if signed.executable { 0o500 } else { 0o400 }),
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
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

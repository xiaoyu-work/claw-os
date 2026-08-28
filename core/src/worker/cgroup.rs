//! cgroup v2 resource governor.
//!
//! When the launcher's own cgroup has a delegated subtree we create a
//! dedicated child cgroup per launch, write the policy's ceilings into
//! it, and move the sandbox into it before `exec`. That gives us
//! kernel-enforced memory / CPU / task limits and, more importantly,
//! `cgroup.kill`: one write tears down every descendant atomically, so
//! a worker cannot survive cancellation by daemonizing.
//!
//! Delegation is not always present (containers, WSL, CI runners). The
//! provider falls back to POSIX rlimits in that case and records which
//! governor actually ran in the audit facts — it never silently drops
//! the ceiling.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

use super::policy::Limits;

const MOUNT_POINT: &str = "/sys/fs/cgroup";

/// A per-launch cgroup, removed on drop.
#[derive(Debug)]
pub struct Scope {
    path: PathBuf,
}

impl Scope {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Move `pid` into this cgroup. Called from the launcher after
    /// `fork` but before the worker can do anything meaningful,
    /// because bubblewrap's own setup happens after we write here.
    pub fn attach(&self, pid: u32) -> Result<(), String> {
        std::fs::write(self.path.join("cgroup.procs"), pid.to_string())
            .map_err(|error| format!("attach worker to cgroup: {error}"))
    }

    /// Kill every process in the cgroup, including descendants that
    /// re-parented or double-forked.
    pub fn kill(&self) {
        let _ = std::fs::write(self.path.join("cgroup.kill"), "1");
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.kill();
        // The kernel refuses rmdir until the cgroup is empty; a couple
        // of retries covers the gap between SIGKILL and reaping.
        for _ in 0..20 {
            if std::fs::remove_dir(&self.path).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

/// Absolute path of the calling process's cgroup, or `None` when the
/// host is not running cgroup v2.
fn current_cgroup() -> Option<PathBuf> {
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let relative = content
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim()
        .trim_start_matches('/')
        .to_string();
    Some(PathBuf::from(MOUNT_POINT).join(relative))
}

/// Can we create and populate a child cgroup for this launch?
///
/// Requires cgroup v2, a delegated (writable) subtree, and the
/// controllers whose limits the policy actually sets.
pub fn is_available() -> bool {
    delegated_parent().is_some()
}

fn delegated_parent() -> Option<PathBuf> {
    if !Path::new(MOUNT_POINT).join("cgroup.controllers").is_file() {
        return None;
    }
    let current = current_cgroup()?;
    // Writing `cgroup.subtree_control` is what "delegated" means: a
    // process that cannot enable controllers for its children cannot
    // impose a ceiling on them either.
    let probe = current.join("cgroup.subtree_control");
    if !probe.is_file() {
        return None;
    }
    // A leaf cgroup that already holds processes cannot gain children
    // without moving them, so only a delegated subtree qualifies.
    let writable = std::fs::OpenOptions::new()
        .append(true)
        .open(&probe)
        .is_ok();
    writable.then_some(current)
}

/// Create the per-launch cgroup and apply `limits`.
pub fn create(name: &str, limits: &Limits) -> Result<Scope, String> {
    let parent =
        delegated_parent().ok_or_else(|| "cgroup v2 delegation is unavailable".to_string())?;
    let _ = std::fs::write(parent.join("cgroup.subtree_control"), "+memory +pids +cpu");
    let path = parent.join(name);
    std::fs::create_dir(&path).map_err(|error| format!("create worker cgroup: {error}"))?;
    let scope = Scope { path };

    write_limit(&scope, "memory.max", &limits.memory_bytes.to_string())?;
    write_limit(&scope, "memory.swap.max", "0")?;
    write_limit(&scope, "pids.max", &limits.pids_max.to_string())?;
    // `cpu.max` is "<quota> <period>"; the policy speaks percent of a
    // single core over the default 100 ms period.
    write_limit(
        &scope,
        "cpu.max",
        &format!("{} 100000", (limits.cpu_percent as u64) * 1000),
    )?;
    Ok(scope)
}

/// Controller files are best-effort: a kernel without `memory.swap.max`
/// (swap accounting disabled) must not fail an otherwise enforceable
/// launch, but a missing `memory.max` or `pids.max` does.
fn write_limit(scope: &Scope, file: &str, value: &str) -> Result<(), String> {
    let path = scope.path.join(file);
    let required = matches!(file, "memory.max" | "pids.max");
    match std::fs::write(&path, value) {
        Ok(()) => Ok(()),
        Err(error) if required => Err(format!("apply worker {file}: {error}")),
        Err(_) => Ok(()),
    }
}

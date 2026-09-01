//! Per-child PID/proc/filesystem isolation for dynamic extensions.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ENABLE_ENV: &str = "COS_EXTENSION_CHILD_ISOLATION";
const MAX_SNAPSHOT_FILES: usize = 4_096;
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SNAPSHOT_DEPTH: usize = 32;

#[derive(Debug)]
pub(crate) struct IsolatedLaunch {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

pub(crate) fn prepare(
    program: impl AsRef<OsStr>,
    initial_args: impl IntoIterator<Item = OsString>,
    authorized_root: Option<&Path>,
) -> Result<IsolatedLaunch, String> {
    let program = program.as_ref().to_os_string();
    let initial_args = initial_args.into_iter().collect::<Vec<_>>();
    if std::env::var(ENABLE_ENV).as_deref() != Ok("1") {
        return Ok(IsolatedLaunch {
            program,
            args: initial_args,
            env: Vec::new(),
        });
    }
    let control = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "extension child isolation requires a task-local HOME".to_string())?;
    let child_root = control
        .join("children")
        .join(uuid::Uuid::new_v4().simple().to_string());
    for child in ["home", "data", "cache", "log", "snapshot"] {
        let path = child_root.join(child);
        fs::create_dir_all(&path)
            .map_err(|error| format!("create isolated child state: {error}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect isolated child state: {error}"))?;
    }
    let usr = fs::symlink_metadata("/usr")
        .map_err(|error| format!("inspect trusted runtime tree: {error}"))?;
    if !usr.is_dir() || usr.file_type().is_symlink() || usr.uid() != 0 || usr.mode() & 0o022 != 0 {
        return Err("trusted runtime tree has unsafe ownership or mode".to_string());
    }

    let mut args = vec![
        "--die-with-parent".into(),
        "--new-session".into(),
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--tmpfs".into(),
        "/".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--dir".into(),
        "/var".into(),
        "--tmpfs".into(),
        "/var/tmp".into(),
        "--dir".into(),
        "/run".into(),
        "--dir".into(),
        "/state".into(),
        "--ro-bind".into(),
        "/usr".into(),
        "/usr".into(),
        "--symlink".into(),
        "usr/bin".into(),
        "/bin".into(),
        "--symlink".into(),
        "usr/sbin".into(),
        "/sbin".into(),
        "--symlink".into(),
        "usr/lib".into(),
        "/lib".into(),
        "--symlink".into(),
        "usr/lib64".into(),
        "/lib64".into(),
    ];
    for child in ["home", "data", "cache", "log"] {
        args.extend([
            "--bind".into(),
            child_root.join(child).as_os_str().to_os_string(),
            format!("/state/{child}").into(),
        ]);
    }
    for path in [
        "/etc/ca-certificates",
        "/etc/hosts",
        "/etc/ld.so.cache",
        "/etc/localtime",
        "/etc/nsswitch.conf",
        "/etc/passwd",
        "/etc/group",
        "/etc/resolv.conf",
        "/etc/ssl",
    ] {
        if Path::new(path).exists() {
            args.extend(["--ro-bind".into(), path.into(), path.into()]);
        }
    }

    let mut roots = Vec::new();
    let program_path = PathBuf::from(&program);
    if program_path.is_absolute() && !program_path.starts_with("/usr") {
        roots.push(program_path);
    }
    if let Some(root) = authorized_root {
        roots.push(root.to_path_buf());
    }
    for key in ["COS_SDK_PYTHON_DIR"] {
        if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
            roots.push(path);
        }
    }
    roots.sort();
    roots.dedup();
    let mut budget = SnapshotBudget {
        files: 0,
        bytes: 0,
        root_dev: None,
    };
    for (index, root) in roots.iter().enumerate() {
        let canonical = root
            .canonicalize()
            .map_err(|error| format!("canonicalize authorized extension path: {error}"))?;
        if canonical.starts_with("/usr") {
            continue;
        }
        let snapshot = child_root.join("snapshot").join(index.to_string());
        snapshot_path(&canonical, &snapshot, 0, &mut budget)?;
        args.extend([
            "--ro-bind".into(),
            snapshot.as_os_str().to_os_string(),
            canonical.as_os_str().to_os_string(),
        ]);
    }

    if let Some(path) = std::env::var_os("COS_PROC_DATA_DIR").map(PathBuf::from) {
        bind_live_read_only(&path, &mut args)?;
    }
    if let Some(path) = std::env::var_os("COS_EXTENSION_BROKER_SOCKET").map(PathBuf::from) {
        bind_live_read_only(&path, &mut args)?;
    }
    for (key, value) in [
        ("HOME", "/state/home"),
        ("COS_HOME", "/state/home"),
        ("COS_DATA_DIR", "/state/data"),
        ("COS_CACHE_DIR", "/state/cache"),
        ("COS_LOG_DIR", "/state/log"),
        ("TMPDIR", "/tmp"),
        ("TMP", "/tmp"),
        ("TEMP", "/tmp"),
    ] {
        args.extend(["--setenv".into(), key.into(), value.into()]);
    }
    args.extend(["--chdir".into(), "/state".into(), "--".into(), program]);
    args.extend(initial_args);
    Ok(IsolatedLaunch {
        program: "/usr/bin/bwrap".into(),
        args,
        env: Vec::new(),
    })
}

fn bind_live_read_only(path: &Path, args: &mut Vec<OsString>) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("canonicalize extension live path: {error}"))?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("inspect extension live path: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("extension live path is a symlink".to_string());
    }
    args.extend([
        "--ro-bind".into(),
        canonical.as_os_str().to_os_string(),
        canonical.as_os_str().to_os_string(),
    ]);
    Ok(())
}

struct SnapshotBudget {
    files: usize,
    bytes: u64,
    root_dev: Option<u64>,
}

fn snapshot_path(
    source: &Path,
    destination: &Path,
    depth: usize,
    budget: &mut SnapshotBudget,
) -> Result<(), String> {
    if depth > MAX_SNAPSHOT_DEPTH {
        return Err("authorized extension snapshot is too deep".to_string());
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect authorized extension path: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("authorized extension path contains a symlink".to_string());
    }
    let owner_uid = std::env::var("COS_EXTENSION_WORKER_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| unsafe { libc::geteuid() as u32 });
    if !matches!(metadata.uid(), 0) && metadata.uid() != owner_uid || metadata.mode() & 0o022 != 0 {
        return Err("authorized extension path has unsafe ownership or mode".to_string());
    }
    match budget.root_dev {
        Some(device) if device != metadata.dev() => {
            return Err("authorized extension snapshot crosses a mount".to_string())
        }
        None => budget.root_dev = Some(metadata.dev()),
        _ => {}
    }
    if metadata.is_dir() {
        fs::create_dir(destination)
            .map_err(|error| format!("create extension snapshot directory: {error}"))?;
        for entry in fs::read_dir(source)
            .map_err(|error| format!("list authorized extension path: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("read authorized extension path: {error}"))?;
            snapshot_path(
                &entry.path(),
                &destination.join(entry.file_name()),
                depth + 1,
                budget,
            )?;
        }
        fs::set_permissions(destination, fs::Permissions::from_mode(0o500))
            .map_err(|error| format!("protect extension snapshot directory: {error}"))?;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err("authorized extension path contains a special file".to_string());
    }
    budget.files = budget.files.saturating_add(1);
    budget.bytes = budget.bytes.saturating_add(metadata.len());
    if budget.files > MAX_SNAPSHOT_FILES || budget.bytes > MAX_SNAPSHOT_BYTES {
        return Err("authorized extension snapshot exceeds its size limit".to_string());
    }
    let mut source_file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source)
        .map_err(|error| format!("open authorized extension file: {error}"))?;
    let opened = source_file
        .metadata()
        .map_err(|error| format!("verify authorized extension file: {error}"))?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() || !opened.is_file() {
        return Err("authorized extension file changed during snapshot".to_string());
    }
    let mut destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|error| format!("create authorized extension snapshot: {error}"))?;
    io::copy(&mut source_file, &mut destination_file)
        .map_err(|error| format!("copy authorized extension file: {error}"))?;
    destination_file
        .sync_all()
        .map_err(|error| format!("sync authorized extension file: {error}"))?;
    let mode = if metadata.mode() & 0o111 != 0 {
        0o500
    } else {
        0o400
    };
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("protect authorized extension file: {error}"))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/child_isolation.rs"
    ));
}

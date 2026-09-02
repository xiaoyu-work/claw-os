//! Per-child PID/proc/filesystem isolation for dynamic extensions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use super::protocol::{ApprovedPath, ExtensionBinding};

const ENABLE_ENV: &str = "COS_EXTENSION_CHILD_ISOLATION";
const MAX_SNAPSHOT_FILES: usize = 4_096;
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SNAPSHOT_DEPTH: usize = 32;
const MAX_RUNTIME_FILES: usize = 100_000;
const MAX_RUNTIME_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_INNER_ENV_COUNT: usize = 128;
const MAX_INNER_ENV_TOTAL: usize = 64 * 1024;
const MAX_INNER_ENV_VALUE: usize = 16 * 1024;
const EXTENSION_UID_START: u32 = 61_000;
const EXTENSION_UID_END: u32 = 61_063;

#[derive(Debug)]
pub(crate) struct IsolatedLaunch {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub isolated: bool,
}

pub(crate) fn close_unallowlisted_fds(command: &mut std::process::Command) {
    unsafe {
        command.pre_exec(|| {
            crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
            Ok(())
        });
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IsolationAuthority {
    task_id: String,
    session_id: Option<String>,
    capability_generation: String,
    owner_uid: u32,
    extension_uid: u32,
    execution_gid: u32,
    approved_paths: Vec<ApprovedPath>,
}

impl IsolationAuthority {
    pub(crate) fn from_binding(binding: &ExtensionBinding) -> Result<Self, String> {
        binding.validate_host(std::process::id(), binding.host_start_time_ticks)?;
        let authority = Self {
            task_id: binding.task_id.clone(),
            session_id: binding.session_id.clone(),
            capability_generation: binding.capability_generation.clone(),
            owner_uid: binding.owner_uid,
            extension_uid: binding.extension_uid,
            execution_gid: binding.owner_gid,
            approved_paths: binding.approved_paths.clone(),
        };
        authority.validate_current()?;
        Ok(authority)
    }

    fn validate_current(&self) -> Result<(), String> {
        if self.task_id.is_empty()
            || self.capability_generation.len() != 16
            || self.owner_uid == 0
            || (cfg!(not(test)) && self.extension_uid == 0)
            || self.execution_gid == 0
            || (cfg!(not(test)) && self.owner_uid == self.extension_uid)
            || (cfg!(not(test)) && unsafe { libc::geteuid() as u32 } != self.extension_uid)
            || (cfg!(not(test)) && unsafe { libc::getegid() as u32 } != self.execution_gid)
        {
            return Err("extension child isolation authority is invalid".to_string());
        }
        let _ = &self.session_id;
        Ok(())
    }

    fn authorize_root(&self, path: &Path) -> Result<PathBuf, String> {
        self.validate_current()?;
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("canonicalize authorized extension root: {error}"))?;
        if is_root_owned_system_extension_root(&canonical)? {
            return Ok(canonical);
        }
        for approved in &self.approved_paths {
            let approved_path = Path::new(&approved.path);
            let metadata = fs::symlink_metadata(approved_path).map_err(|error| {
                format!("revalidate broker-approved path {}: {error}", approved.path)
            })?;
            if metadata.file_type().is_symlink()
                || metadata.dev() != approved.device
                || metadata.ino() != approved.inode
                || metadata.uid() != approved.owner_uid
                || metadata.mode() != approved.mode
            {
                return Err(format!(
                    "broker-approved path identity changed: {}",
                    approved.path
                ));
            }
            if canonical.starts_with(approved_path) {
                return Ok(canonical);
            }
        }
        Err(format!(
            "extension root {} is outside broker-approved owner/package paths",
            canonical.display()
        ))
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        owner_uid: u32,
        execution_gid: u32,
        approved_paths: Vec<ApprovedPath>,
    ) -> Self {
        Self {
            task_id: "test-task".to_string(),
            session_id: Some("test-session".to_string()),
            capability_generation: "a".repeat(16),
            owner_uid,
            extension_uid: unsafe { libc::geteuid() as u32 },
            execution_gid,
            approved_paths,
        }
    }
}

fn is_root_owned_system_extension_root(path: &Path) -> Result<bool, String> {
    if !path.starts_with("/opt")
        && !path.starts_with("/usr/lib/cos")
        && !path.starts_with("/usr/share/cos")
    {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect system extension path {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "system extension path {} has unsafe ownership or mode",
            path.display()
        ));
    }
    Ok(true)
}

pub(crate) fn prepare(
    program: impl AsRef<OsStr>,
    initial_args: impl IntoIterator<Item = OsString>,
    authorized_root: Option<&Path>,
    authority: Option<&IsolationAuthority>,
) -> Result<IsolatedLaunch, String> {
    prepare_impl(
        program,
        initial_args,
        authorized_root,
        Vec::new(),
        false,
        authority,
        IsolationOptions::dynamic_extension(),
    )
}

pub(crate) fn prepare_with_clean_env(
    program: impl AsRef<OsStr>,
    initial_args: impl IntoIterator<Item = OsString>,
    authorized_root: Option<&Path>,
    inner_env: Vec<(OsString, OsString)>,
    authority: Option<&IsolationAuthority>,
) -> Result<IsolatedLaunch, String> {
    validate_inner_environment(&inner_env)?;
    prepare_impl(
        program,
        initial_args,
        authorized_root,
        inner_env,
        true,
        authority,
        IsolationOptions::dynamic_extension(),
    )
}

pub(crate) fn prepare_verified_package(
    program: impl AsRef<OsStr>,
    verified_root: &Path,
    inner_env: Vec<(OsString, OsString)>,
    authority: &IsolationAuthority,
) -> Result<IsolatedLaunch, String> {
    validate_inner_environment(&inner_env)?;
    prepare_impl(
        program,
        Vec::<OsString>::new(),
        Some(verified_root),
        inner_env,
        true,
        Some(authority),
        IsolationOptions::agent_extension(),
    )
}

#[derive(Clone, Copy)]
struct IsolationOptions {
    verified_owner_source: bool,
    expose_provider_authority: bool,
}

impl IsolationOptions {
    const fn dynamic_extension() -> Self {
        Self {
            verified_owner_source: false,
            expose_provider_authority: false,
        }
    }

    const fn agent_extension() -> Self {
        Self {
            verified_owner_source: true,
            expose_provider_authority: false,
        }
    }
}

fn prepare_impl(
    program: impl AsRef<OsStr>,
    initial_args: impl IntoIterator<Item = OsString>,
    authorized_root: Option<&Path>,
    inner_env: Vec<(OsString, OsString)>,
    clear_inner_environment: bool,
    authority: Option<&IsolationAuthority>,
    options: IsolationOptions,
) -> Result<IsolatedLaunch, String> {
    let program = program.as_ref().to_os_string();
    let mut initial_args = initial_args.into_iter().collect::<Vec<_>>();
    if std::env::var(ENABLE_ENV).as_deref() != Ok("1") {
        return Ok(IsolatedLaunch {
            program,
            args: initial_args,
            env: inner_env,
            isolated: false,
        });
    }
    let authority = authority
        .ok_or_else(|| "extension child isolation requires typed runtime authority".to_string())?;
    authority.validate_current()?;
    let control = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "extension child isolation requires a task-local HOME".to_string())?;
    let child_root = control
        .join("children")
        .join(uuid::Uuid::new_v4().simple().to_string());
    for child in ["home", "data", "cache", "log", "snapshot", "runtime"] {
        let path = child_root.join(child);
        fs::create_dir_all(&path)
            .map_err(|error| format!("create isolated child state: {error}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect isolated child state: {error}"))?;
    }
    let authorized_root = authorized_root
        .map(|root| {
            if options.verified_owner_source {
                validate_verified_package_root(root, &control, authority)
            } else {
                authority.authorize_root(root)
            }
        })
        .transpose()?;
    let resolved_program =
        resolve_runtime_program(Path::new(&program), authorized_root.as_deref(), authority)?;
    let mut runtime_programs = vec![resolved_program.clone()];
    if initial_args.first().is_some_and(|value| value == "--") && initial_args.len() >= 2 {
        let inner = resolve_runtime_program(
            Path::new(&initial_args[1]),
            authorized_root.as_deref(),
            authority,
        )?;
        initial_args[1] = inner.as_os_str().to_os_string();
        runtime_programs.push(inner);
    }
    if let Some(cos_bin) = std::env::var_os("COS_BIN").map(PathBuf::from) {
        if cos_bin.starts_with("/usr/local/bin") {
            runtime_programs.push(resolve_runtime_program(&cos_bin, None, authority)?);
        }
    }

    let mut args = vec![
        "--die-with-parent".into(),
        "--new-session".into(),
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-net".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--tmpfs".into(),
        "/".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/dev/shm".into(),
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
        "--dir".into(),
        "/usr".into(),
        "--dir".into(),
        "/usr/bin".into(),
        "--dir".into(),
        "/usr/sbin".into(),
        "--dir".into(),
        "/usr/lib".into(),
        "--dir".into(),
        "/usr/lib64".into(),
        "--dir".into(),
        "/usr/share".into(),
        "--dir".into(),
        "/usr/local".into(),
        "--dir".into(),
        "/usr/local/bin".into(),
        "--dir".into(),
        "/lib".into(),
        "--dir".into(),
        "/lib64".into(),
        "--dir".into(),
        "/etc".into(),
        "--symlink".into(),
        "usr/bin".into(),
        "/bin".into(),
        "--symlink".into(),
        "usr/sbin".into(),
        "/sbin".into(),
    ];
    if clear_inner_environment {
        args.push("--clearenv".into());
        for (key, value) in inner_env {
            args.extend(["--setenv".into(), key, value]);
        }
    }
    add_minimal_runtime(
        &runtime_programs,
        &child_root.join("runtime"),
        &mut args,
        authority,
    )?;
    for child in ["home", "data", "cache", "log"] {
        args.extend([
            "--bind".into(),
            child_root.join(child).as_os_str().to_os_string(),
            format!("/state/{child}").into(),
        ]);
    }
    create_minimal_etc(&child_root, &mut args)?;

    let mut roots = Vec::new();
    let program_path = PathBuf::from(&program);
    if !options.verified_owner_source
        && program_path.is_absolute()
        && !program_path.starts_with("/usr")
    {
        roots.push(program_path);
    }
    if let Some(root) = authorized_root.as_deref() {
        roots.push(root.to_path_buf());
    }
    if options.expose_provider_authority {
        for key in ["COS_SDK_PYTHON_DIR"] {
            if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
                roots.push(path);
            }
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
        if fs::symlink_metadata(root)
            .map_err(|error| format!("inspect authorized extension root: {error}"))?
            .file_type()
            .is_symlink()
        {
            return Err("authorized extension root must not be a symlink".to_string());
        }
        let canonical = if options.verified_owner_source {
            validate_verified_package_root(root, &control, authority)?
        } else {
            authority.authorize_root(root)?
        };
        let snapshot = child_root.join("snapshot").join(index.to_string());
        // Mount boundaries are forbidden within one authorized tree, but
        // independent approved roots may legitimately live on different
        // filesystems.
        budget.root_dev = None;
        snapshot_path(
            &canonical,
            &snapshot,
            0,
            &mut budget,
            authority,
            options.verified_owner_source,
        )?;
        args.extend([
            "--ro-bind".into(),
            snapshot.as_os_str().to_os_string(),
            canonical.as_os_str().to_os_string(),
        ]);
    }

    if options.expose_provider_authority {
        if let Some(path) = std::env::var_os("COS_PROC_DATA_DIR").map(PathBuf::from) {
            bind_live_read_only(&path, &mut args)?;
        }
        if let Some(path) = std::env::var_os("COS_EXTENSION_BROKER_SOCKET").map(PathBuf::from) {
            bind_live_read_only(&path, &mut args)?;
        }
    }
    for (key, value) in [
        ("HOME", "/state/home"),
        ("COS_HOME", "/state/home"),
        ("COS_DATA_DIR", "/state/data"),
        ("COS_CACHE_DIR", "/state/cache"),
        ("COS_LOG_DIR", "/state/log"),
        ("COS_EXTENSION_CHILD_ISOLATION", "1"),
        ("TMPDIR", "/tmp"),
        ("TMP", "/tmp"),
        ("TEMP", "/tmp"),
    ] {
        args.extend(["--setenv".into(), key.into(), value.into()]);
    }
    let inner_cwd = match authorized_root {
        Some(root) => root,
        None => PathBuf::from("/state"),
    };
    args.extend([
        "--chdir".into(),
        inner_cwd.as_os_str().to_os_string(),
        "--".into(),
        resolved_program.into_os_string(),
    ]);
    args.extend(initial_args);
    Ok(IsolatedLaunch {
        program: "/usr/bin/bwrap".into(),
        args,
        env: Vec::new(),
        isolated: true,
    })
}

fn validate_inner_environment(environment: &[(OsString, OsString)]) -> Result<(), String> {
    if environment.len() > MAX_INNER_ENV_COUNT {
        return Err("extension environment has too many entries".to_string());
    }
    let mut total = 0usize;
    for (key, value) in environment {
        let Some(key) = key.to_str() else {
            return Err("extension environment key is not UTF-8".to_string());
        };
        let Some(value) = value.to_str() else {
            return Err(format!(
                "extension environment value for `{key}` is not UTF-8"
            ));
        };
        let mut bytes = key.bytes();
        if key.is_empty()
            || key.len() > 128
            || !bytes
                .next()
                .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
            || !bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            return Err(format!("invalid extension environment key `{key}`"));
        }
        if key.starts_with("LD_") {
            return Err(format!(
                "loader-control environment key `{key}` is not permitted"
            ));
        }
        if value.as_bytes().contains(&0) || value.len() > MAX_INNER_ENV_VALUE {
            return Err(format!(
                "extension environment value for `{key}` is invalid"
            ));
        }
        total = total
            .checked_add(key.len() + value.len() + 2)
            .ok_or_else(|| "extension environment size overflow".to_string())?;
        if total > MAX_INNER_ENV_TOTAL {
            return Err("extension environment exceeds its size limit".to_string());
        }
    }
    Ok(())
}

fn resolve_runtime_program(
    command: &Path,
    authorized_root: Option<&Path>,
    authority: &IsolationAuthority,
) -> Result<PathBuf, String> {
    let candidate = if command.is_absolute() {
        command.to_path_buf()
    } else {
        if command.components().count() != 1 {
            return Err("extension command must be absolute or a bare executable name".to_string());
        }
        ["/usr/bin", "/bin"]
            .into_iter()
            .map(|directory| Path::new(directory).join(command))
            .find(|path| path.is_file())
            .ok_or_else(|| format!("extension executable `{}` was not found", command.display()))?
    };
    let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
        format!(
            "inspect extension executable {}: {error}",
            candidate.display()
        )
    })?;
    let system_candidate = candidate.starts_with("/usr/bin")
        || candidate.starts_with("/bin")
        || matches!(
            candidate.to_str(),
            Some("/usr/local/bin/claw-app-runner" | "/usr/local/bin/cos")
        );
    if metadata.file_type().is_symlink() && !system_candidate {
        return Err("custom extension executable must not be a symlink".to_string());
    }
    let canonical = candidate.canonicalize().map_err(|error| {
        format!(
            "resolve extension executable {}: {error}",
            candidate.display()
        )
    })?;
    let authorized = authorized_root.is_some_and(|root| canonical.starts_with(root));
    if !authorized && !is_system_runtime_path(&canonical) {
        authority.authorize_root(&canonical)?;
    }
    if !canonical.is_file() {
        return Err(format!(
            "extension executable {} is not a regular file",
            canonical.display()
        ));
    }
    let canonical_metadata = fs::metadata(&canonical)
        .map_err(|error| format!("inspect resolved extension executable: {error}"))?;
    if canonical_metadata.mode() & 0o111 == 0 {
        return Err(format!(
            "extension executable {} is not executable",
            canonical.display()
        ));
    }
    if !authorized
        && candidate.starts_with("/opt")
        && (canonical_metadata.uid() != 0 || canonical_metadata.mode() & 0o022 != 0)
    {
        return Err(
            "system-wide custom extension executables must be root-owned and non-writable"
                .to_string(),
        );
    }
    Ok(canonical)
}

fn add_minimal_runtime(
    programs: &[PathBuf],
    runtime_snapshot: &Path,
    args: &mut Vec<OsString>,
    authority: &IsolationAuthority,
) -> Result<(), String> {
    let mut system_programs = Vec::new();
    for program in programs {
        collect_script_interpreters(program, &mut system_programs, authority)?;
        if is_system_runtime_path(program) {
            system_programs.push(program.clone());
        }
    }
    system_programs.sort();
    system_programs.dedup();
    let mut runtime_files = BTreeMap::<PathBuf, PathBuf>::new();
    for program in &system_programs {
        validate_runtime_file(program, authority)?;
        runtime_files.insert(program.clone(), program.clone());
        if program
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("python3."))
        {
            for alias in ["/usr/bin/python3", "/usr/bin/python"] {
                runtime_files.insert(alias.into(), program.clone());
            }
        }
    }

    if system_programs.iter().any(|program| {
        program
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("python"))
    }) {
        let version = system_programs
            .iter()
            .filter_map(|program| program.file_name().and_then(OsStr::to_str))
            .find_map(|name| name.strip_prefix("python"))
            .filter(|version| !version.is_empty() && version.contains('.'))
            .unwrap_or("3");
        let stdlib = PathBuf::from(format!("/usr/lib/python{version}"));
        if stdlib.exists() {
            snapshot_runtime_tree(
                &stdlib,
                &runtime_snapshot.join(format!("python{version}")),
                &stdlib,
                args,
                authority,
            )?;
            collect_tree_elf_dependencies(&stdlib, &mut runtime_files)?;
        }
    }
    if system_programs
        .iter()
        .any(|program| program.file_name().and_then(OsStr::to_str) == Some("node"))
    {
        let node_modules = Path::new("/usr/share/nodejs");
        if node_modules.exists() {
            snapshot_runtime_tree(
                node_modules,
                &runtime_snapshot.join("nodejs"),
                node_modules,
                args,
                authority,
            )?;
            collect_tree_elf_dependencies(node_modules, &mut runtime_files)?;
        }
    }
    collect_elf_dependencies(programs, &mut runtime_files)?;
    collect_elf_dependencies(&system_programs, &mut runtime_files)?;
    for library in ["libnss_files.so.2", "libnss_dns.so.2"] {
        if let Some((destination, source)) = resolve_runtime_library(library)? {
            runtime_files.insert(destination, source);
        }
    }
    for (destination, source) in runtime_files {
        validate_runtime_file(&source, authority)?;
        args.extend([
            "--ro-bind".into(),
            source.into_os_string(),
            destination.into_os_string(),
        ]);
    }
    Ok(())
}

fn collect_elf_dependencies(
    roots: &[PathBuf],
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), String> {
    let mut queue = VecDeque::from(roots.to_vec());
    let mut visited = BTreeSet::new();
    while let Some(path) = queue.pop_front() {
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("resolve runtime ELF {}: {error}", path.display()))?;
        if !visited.insert(canonical.clone()) {
            continue;
        }
        let metadata = fs::metadata(&canonical)
            .map_err(|error| format!("inspect runtime ELF {}: {error}", canonical.display()))?;
        if metadata.len() > MAX_SNAPSHOT_BYTES {
            return Err(format!(
                "runtime ELF {} exceeds its size limit",
                canonical.display()
            ));
        }
        let bytes = fs::read(&canonical)
            .map_err(|error| format!("read runtime ELF {}: {error}", canonical.display()))?;
        let Ok(elf) = goblin::elf::Elf::parse(&bytes) else {
            continue;
        };
        if let Some(interpreter) = elf.interpreter {
            let destination = PathBuf::from(interpreter);
            let source = destination
                .canonicalize()
                .map_err(|error| format!("resolve runtime loader {interpreter}: {error}"))?;
            files.insert(destination, source.clone());
            queue.push_back(source);
        }
        for library in elf.libraries {
            let Some((destination, source)) = resolve_runtime_library(library)? else {
                return Err(format!(
                    "required runtime library `{library}` for {} was not found",
                    canonical.display()
                ));
            };
            files.insert(destination, source.clone());
            queue.push_back(source);
        }
    }
    Ok(())
}

fn collect_tree_elf_dependencies(
    root: &Path,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), String> {
    let mut candidates = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(path) = queue.pop() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect runtime dependency candidate: {error}"))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)
                .map_err(|error| format!("list runtime dependency tree: {error}"))?
            {
                queue.push(
                    entry
                        .map_err(|error| format!("read runtime dependency tree: {error}"))?
                        .path(),
                );
            }
        } else if metadata.is_file() {
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
            if name.ends_with(".node") || name.contains(".so") {
                candidates.push(path);
            }
        }
    }
    collect_elf_dependencies(&candidates, files)
}

fn resolve_runtime_library(name: &str) -> Result<Option<(PathBuf, PathBuf)>, String> {
    if name.contains('/') || name.is_empty() {
        return Err(format!("invalid runtime library name `{name}`"));
    }
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        other => {
            return Err(format!(
                "unsupported extension runtime architecture `{other}`"
            ))
        }
    };
    for directory in [
        format!("/usr/lib/{architecture}"),
        format!("/lib/{architecture}"),
        "/usr/lib64".to_string(),
        "/lib64".to_string(),
        "/usr/lib".to_string(),
        "/lib".to_string(),
    ] {
        let destination = PathBuf::from(directory).join(name);
        if destination.exists() {
            let source = destination
                .canonicalize()
                .map_err(|error| format!("resolve runtime library `{name}`: {error}"))?;
            return Ok(Some((destination, source)));
        }
    }
    Ok(None)
}

fn collect_script_interpreters(
    program: &Path,
    out: &mut Vec<PathBuf>,
    authority: &IsolationAuthority,
) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(program)
        .map_err(|error| format!("read extension executable {}: {error}", program.display()))?;
    let mut content = vec![0u8; 4096];
    let read = file
        .read(&mut content)
        .map_err(|error| format!("read extension executable header: {error}"))?;
    content.truncate(read);
    if !content.starts_with(b"#!") {
        return Ok(());
    }
    let line_end = content
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "extension script interpreter line is too long".to_string())?;
    let line = std::str::from_utf8(&content[2..line_end])
        .map_err(|_| "extension script interpreter is not UTF-8".to_string())?
        .trim();
    let interpreter = line
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| "extension script has an empty interpreter".to_string())?;
    if interpreter == "/usr/bin/env" {
        return Err(
            "extension scripts using `/usr/bin/env` are not supported in isolation".to_string(),
        );
    }
    let resolved = resolve_runtime_program(Path::new(interpreter), None, authority)?;
    if !is_system_runtime_path(&resolved) {
        return Err("extension script interpreter is outside the trusted runtime".to_string());
    }
    out.push(resolved);
    Ok(())
}

fn is_system_runtime_path(path: &Path) -> bool {
    path.starts_with("/usr/bin")
        || path.starts_with("/usr/sbin")
        || path.starts_with("/usr/lib")
        || matches!(
            path.to_str(),
            Some("/usr/local/bin/claw-app-runner" | "/usr/local/bin/cos")
        )
}

fn validate_runtime_file(path: &Path, authority: &IsolationAuthority) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect runtime file {}: {error}", path.display()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || (EXTENSION_UID_START..=EXTENSION_UID_END).contains(&metadata.uid())
        || metadata.gid() == authority.execution_gid
    {
        return Err(format!(
            "runtime file {} has unsafe type, ownership, or mode",
            path.display()
        ));
    }
    Ok(())
}

fn snapshot_runtime_tree(
    source: &Path,
    snapshot: &Path,
    destination: &Path,
    args: &mut Vec<OsString>,
    authority: &IsolationAuthority,
) -> Result<(), String> {
    let canonical = source
        .canonicalize()
        .map_err(|error| format!("resolve runtime tree {}: {error}", source.display()))?;
    if canonical.starts_with("/usr/local") {
        return Err("broad `/usr/local` runtime trees are forbidden".to_string());
    }
    let root = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("inspect runtime tree {}: {error}", canonical.display()))?;
    if !root.is_dir()
        || root.file_type().is_symlink()
        || root.uid() != 0
        || root.mode() & 0o022 != 0
    {
        return Err(format!(
            "runtime tree {} has unsafe root metadata",
            canonical.display()
        ));
    }
    let mut budget = RuntimeSnapshotBudget { files: 0, bytes: 0 };
    snapshot_runtime_entry(&canonical, snapshot, root.dev(), &mut budget, authority)?;
    let after = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("recheck runtime tree {}: {error}", canonical.display()))?;
    if after.dev() != root.dev() || after.ino() != root.ino() {
        return Err(format!(
            "runtime tree {} changed during snapshot",
            canonical.display()
        ));
    }
    args.extend([
        "--ro-bind".into(),
        snapshot.as_os_str().to_os_string(),
        destination.as_os_str().to_os_string(),
    ]);
    Ok(())
}

struct RuntimeSnapshotBudget {
    files: usize,
    bytes: u64,
}

fn snapshot_runtime_entry(
    source: &Path,
    destination: &Path,
    device: u64,
    budget: &mut RuntimeSnapshotBudget,
    authority: &IsolationAuthority,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect runtime object {}: {error}", source.display()))?;
    if metadata.dev() != device {
        return Err(format!(
            "runtime tree crosses a mount at {}",
            source.display()
        ));
    }
    // Symlinks never enter the child view. Internal links are unnecessary
    // aliases; escaping links are specifically excluded rather than copied.
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || (EXTENSION_UID_START..=EXTENSION_UID_END).contains(&metadata.uid())
        || metadata.gid() == authority.execution_gid
    {
        return Err(format!(
            "runtime object {} has unsafe ownership or mode",
            source.display()
        ));
    }
    if metadata.is_dir() {
        fs::create_dir(destination)
            .map_err(|error| format!("create runtime snapshot directory: {error}"))?;
        for entry in fs::read_dir(source)
            .map_err(|error| format!("list runtime tree {}: {error}", source.display()))?
        {
            let entry = entry.map_err(|error| format!("read runtime tree: {error}"))?;
            snapshot_runtime_entry(
                &entry.path(),
                &destination.join(entry.file_name()),
                device,
                budget,
                authority,
            )?;
        }
        fs::set_permissions(destination, fs::Permissions::from_mode(0o500))
            .map_err(|error| format!("protect runtime snapshot directory: {error}"))?;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!(
            "runtime tree contains a special file at {}",
            source.display()
        ));
    }
    budget.files = budget.files.saturating_add(1);
    budget.bytes = budget.bytes.saturating_add(metadata.len());
    if budget.files > MAX_RUNTIME_FILES || budget.bytes > MAX_RUNTIME_BYTES {
        return Err("runtime snapshot exceeds its size limit".to_string());
    }
    let mut source_file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source)
        .map_err(|error| format!("open runtime file {}: {error}", source.display()))?;
    let opened = source_file
        .metadata()
        .map_err(|error| format!("verify runtime file {}: {error}", source.display()))?;
    if opened.dev() != metadata.dev()
        || opened.ino() != metadata.ino()
        || opened.len() != metadata.len()
        || !opened.is_file()
    {
        return Err(format!(
            "runtime file {} changed during snapshot",
            source.display()
        ));
    }
    let mut destination_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(destination)
        .map_err(|error| format!("create runtime snapshot file: {error}"))?;
    io::copy(&mut source_file, &mut destination_file)
        .map_err(|error| format!("copy runtime file: {error}"))?;
    destination_file
        .sync_all()
        .map_err(|error| format!("sync runtime snapshot file: {error}"))?;
    let mode = if metadata.mode() & 0o111 != 0 {
        0o500
    } else {
        0o400
    };
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("protect runtime snapshot file: {error}"))
}

fn validate_and_bind_runtime_tree(
    source: &Path,
    destination: &Path,
    args: &mut Vec<OsString>,
    authority: &IsolationAuthority,
) -> Result<(), String> {
    let canonical = source
        .canonicalize()
        .map_err(|error| format!("resolve runtime tree {}: {error}", source.display()))?;
    if canonical.starts_with("/usr/local") {
        return Err("broad `/usr/local` runtime trees are forbidden".to_string());
    }
    let root = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("inspect runtime tree {}: {error}", canonical.display()))?;
    if !root.is_dir()
        || root.file_type().is_symlink()
        || root.uid() != 0
        || root.mode() & 0o022 != 0
    {
        return Err(format!(
            "runtime tree {} has unsafe root metadata",
            canonical.display()
        ));
    }
    let mut count = 0usize;
    validate_runtime_tree_entry(&canonical, &canonical, root.dev(), &mut count, authority)?;
    let after = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("recheck runtime tree {}: {error}", canonical.display()))?;
    if after.dev() != root.dev() || after.ino() != root.ino() {
        return Err(format!(
            "runtime tree {} changed during validation",
            canonical.display()
        ));
    }
    args.extend([
        "--ro-bind".into(),
        canonical.into_os_string(),
        destination.as_os_str().to_os_string(),
    ]);
    Ok(())
}

fn validate_runtime_tree_entry(
    root: &Path,
    path: &Path,
    device: u64,
    count: &mut usize,
    authority: &IsolationAuthority,
) -> Result<(), String> {
    *count = count.saturating_add(1);
    if *count > 100_000 {
        return Err("runtime tree contains too many objects".to_string());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect runtime object {}: {error}", path.display()))?;
    if metadata.dev() != device {
        return Err(format!(
            "runtime tree crosses a mount at {}",
            path.display()
        ));
    }
    if metadata.file_type().is_symlink() {
        let target = path
            .canonicalize()
            .map_err(|error| format!("resolve runtime symlink {}: {error}", path.display()))?;
        if !target.starts_with(root) {
            return Err(format!(
                "runtime symlink escapes its subtree at {}",
                path.display()
            ));
        }
        return Ok(());
    }
    if metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || (EXTENSION_UID_START..=EXTENSION_UID_END).contains(&metadata.uid())
        || metadata.gid() == authority.execution_gid
    {
        return Err(format!(
            "runtime object {} has unsafe ownership or mode",
            path.display()
        ));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|error| format!("list runtime tree {}: {error}", path.display()))?
        {
            let entry = entry.map_err(|error| format!("read runtime tree: {error}"))?;
            validate_runtime_tree_entry(root, &entry.path(), device, count, authority)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!(
            "runtime tree contains a special file at {}",
            path.display()
        ));
    }
    Ok(())
}

fn create_minimal_etc(child_root: &Path, args: &mut Vec<OsString>) -> Result<(), String> {
    let etc = child_root.join("etc");
    fs::create_dir(&etc).map_err(|error| format!("create isolated etc: {error}"))?;
    for (name, body) in [
        ("passwd", "root:x:0:0:extension:/state/home:/bin/false\n"),
        ("group", "root:x:0:\n"),
        (
            "nsswitch.conf",
            "passwd: files\ngroup: files\nhosts: files\n",
        ),
        ("hosts", "127.0.0.1 localhost\n::1 localhost\n"),
    ] {
        let path = etc.join(name);
        fs::write(&path, body).map_err(|error| format!("write isolated {name}: {error}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400))
            .map_err(|error| format!("protect isolated {name}: {error}"))?;
        args.extend([
            "--ro-bind".into(),
            path.into_os_string(),
            format!("/etc/{name}").into(),
        ]);
    }
    Ok(())
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

fn validate_verified_package_root(
    root: &Path,
    control: &Path,
    authority: &IsolationAuthority,
) -> Result<PathBuf, String> {
    let expected = control.join("verified-packages");
    let expected = expected
        .canonicalize()
        .map_err(|error| format!("resolve verified package root: {error}"))?;
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("resolve verified extension package: {error}"))?;
    if !canonical.starts_with(&expected) {
        return Err("verified extension package escaped task-local storage".to_string());
    }
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("inspect verified extension package: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != authority.extension_uid
        || metadata.gid() != authority.execution_gid
        || metadata.mode() & 0o077 != 0
    {
        return Err("verified extension package root has unsafe identity".to_string());
    }
    Ok(canonical)
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
    authority: &IsolationAuthority,
    allow_verified_owner: bool,
) -> Result<(), String> {
    if depth > MAX_SNAPSHOT_DEPTH {
        return Err("authorized extension snapshot is too deep".to_string());
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect authorized extension path: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("authorized extension path contains a symlink".to_string());
    }
    let verified_owner = allow_verified_owner
        && metadata.uid() == authority.extension_uid
        && metadata.gid() == authority.execution_gid;
    if (!verified_owner && metadata.uid() != 0 && metadata.uid() != authority.owner_uid)
        || (!verified_owner && (EXTENSION_UID_START..=EXTENSION_UID_END).contains(&metadata.uid()))
        || (!verified_owner && metadata.gid() == authority.execution_gid)
        || metadata.mode() & 0o022 != 0
    {
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
                authority,
                allow_verified_owner,
            )?;
        }
        fs::set_permissions(destination, fs::Permissions::from_mode(0o500))
            .map_err(|error| format!("protect extension snapshot directory: {error}"))?;
        let after = fs::symlink_metadata(source)
            .map_err(|error| format!("recheck authorized extension directory: {error}"))?;
        if after.dev() != metadata.dev()
            || after.ino() != metadata.ino()
            || after.mtime() != metadata.mtime()
            || after.mtime_nsec() != metadata.mtime_nsec()
            || after.ctime() != metadata.ctime()
            || after.ctime_nsec() != metadata.ctime_nsec()
        {
            return Err("authorized extension directory changed during snapshot".to_string());
        }
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
    let after = source_file
        .metadata()
        .map_err(|error| format!("recheck authorized extension file: {error}"))?;
    if after.len() != metadata.len()
        || after.mtime() != metadata.mtime()
        || after.mtime_nsec() != metadata.mtime_nsec()
        || after.ctime() != metadata.ctime()
        || after.ctime_nsec() != metadata.ctime_nsec()
    {
        return Err("authorized extension file changed during snapshot".to_string());
    }
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

//! Root-owned policy for the narrow unsandboxed process-spawn surface.
//!
//! A static ELF shape is not a security identity: a renamed interpreter,
//! build tool, or plugin loader is still native code. The only unsandboxed
//! commands accepted here are exact root-owned path + content-hash entries
//! whose argv and cwd are fixed by a versioned, root-owned manifest.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    PinnedSpawnDirectory, SpawnExecutionIdentity, SpawnFileIdentity, SpawnResourceBinding,
};

const ALLOWLIST_VERSION: u32 = 1;
const ALLOWLIST_PATH: &str = "/etc/cos/proc-spawn-allowlist.json";
const MAX_ALLOWLIST_BYTES: u64 = 128 * 1024;
const MAX_COMMANDS: usize = 128;
const MAX_ARGS: usize = 64;
const MAX_ARG_BYTES: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Allowlist {
    version: u32,
    commands: Vec<Command>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    id: String,
    executable: String,
    sha256: String,
    #[serde(default)]
    argv_exact: Vec<String>,
    #[serde(default)]
    output_args: Vec<usize>,
    workdir: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct Authorization {
    version: u32,
    command_id: String,
    policy_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OutputBinding {
    arg_index: usize,
    descriptor_role: String,
    path: String,
    parent: SpawnFileIdentity,
    output: OutputIdentity,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct OutputIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner_uid: u32,
    owner_gid: u32,
    size: u64,
    links: u64,
}

pub(super) struct PinnedOutput {
    _parent: fs::File,
    file: fs::File,
}

pub(super) struct AuthorizedInvocation {
    pub authorization: Authorization,
    pub argv: Vec<String>,
    pub output_bindings: Vec<OutputBinding>,
    pub outputs: Vec<PinnedOutput>,
}

impl AuthorizedInvocation {
    pub fn output_fds(&self) -> Vec<RawFd> {
        self.outputs
            .iter()
            .map(|output| output.file.as_raw_fd())
            .collect()
    }
}

struct LoadedAllowlist {
    policy: Allowlist,
    sha256: String,
}

pub(super) fn authorize(
    executable: &SpawnResourceBinding,
    args: &[String],
    workdir: &PinnedSpawnDirectory,
    execution: &SpawnExecutionIdentity,
) -> Result<AuthorizedInvocation, String> {
    let trusted_uid = trusted_owner_uid();
    let workdir_binding = workdir.binding();
    validate_immutable_resource(executable, trusted_uid, "executable")?;
    validate_immutable_resource(&workdir_binding, trusted_uid, "working directory")?;
    let loaded = load(trusted_uid)?;
    validate_policy(&loaded.policy)?;

    let Some(content_sha256) = executable.content_sha256.as_deref() else {
        return Err(refusal("executable content hash is unavailable"));
    };
    let path_matches: Vec<&Command> = loaded
        .policy
        .commands
        .iter()
        .filter(|command| command.executable == executable.path)
        .collect();
    if path_matches.is_empty() {
        return Err(refusal("executable path is not in the audited allowlist"));
    }
    let hash_matches: Vec<&Command> = path_matches
        .into_iter()
        .filter(|command| command.sha256 == content_sha256)
        .collect();
    if hash_matches.is_empty() {
        return Err(refusal(
            "executable content does not match its allowlisted hash",
        ));
    }
    let Some(command) = hash_matches
        .into_iter()
        .find(|command| command.argv_exact == args && command.workdir == workdir_binding.path)
    else {
        return Err(refusal(
            "argv or working directory does not match the command-specific schema",
        ));
    };
    let (argv, outputs, output_bindings) = pin_outputs(command, workdir, execution)?;

    Ok(AuthorizedInvocation {
        authorization: Authorization {
            version: loaded.policy.version,
            command_id: command.id.clone(),
            policy_sha256: loaded.sha256,
        },
        argv,
        output_bindings,
        outputs,
    })
}

fn load(trusted_uid: u32) -> Result<LoadedAllowlist, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let path = allowlist_path();
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| refusal(&format!("read audited allowlist: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(refusal("audited allowlist is not a regular file"));
    }
    if metadata.uid() != trusted_uid || metadata.mode() & 0o022 != 0 {
        return Err(refusal(
            "audited allowlist is not owned and writable only by root",
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_ALLOWLIST_BYTES {
        return Err(refusal("audited allowlist size is invalid"));
    }

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| refusal(&format!("open audited allowlist: {error}")))?;
    let opened = file
        .metadata()
        .map_err(|error| refusal(&format!("inspect audited allowlist: {error}")))?;
    if !opened.is_file()
        || opened.dev() != metadata.dev()
        || opened.ino() != metadata.ino()
        || opened.uid() != trusted_uid
        || opened.mode() & 0o022 != 0
    {
        return Err(refusal("audited allowlist changed while it was opened"));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(MAX_ALLOWLIST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| refusal(&format!("read audited allowlist: {error}")))?;
    if bytes.len() as u64 != opened.len() || bytes.len() as u64 > MAX_ALLOWLIST_BYTES {
        return Err(refusal("audited allowlist changed while it was read"));
    }
    let policy: Allowlist = serde_json::from_slice(&bytes)
        .map_err(|error| refusal(&format!("parse audited allowlist: {error}")))?;
    Ok(LoadedAllowlist {
        policy,
        sha256: crate::crypto::sha256_hex(&bytes),
    })
}

fn validate_policy(policy: &Allowlist) -> Result<(), String> {
    if policy.version != ALLOWLIST_VERSION {
        return Err(refusal("audited allowlist version is unsupported"));
    }
    if policy.commands.len() > MAX_COMMANDS {
        return Err(refusal("audited allowlist contains too many commands"));
    }
    let mut ids = BTreeSet::new();
    for command in &policy.commands {
        if command.id.is_empty()
            || command.id.len() > 64
            || !command
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(refusal("allowlisted command id is invalid"));
        }
        if !ids.insert(command.id.as_str()) {
            return Err(refusal("allowlisted command id is duplicated"));
        }
        validate_canonical_absolute_path(&command.executable, "executable")?;
        validate_canonical_absolute_path(&command.workdir, "working directory")?;
        if !is_sha256(&command.sha256) {
            return Err(refusal("allowlisted executable hash is invalid"));
        }
        if command.argv_exact.len() > MAX_ARGS
            || command
                .argv_exact
                .iter()
                .any(|arg| arg.len() > MAX_ARG_BYTES || arg.as_bytes().contains(&0))
        {
            return Err(refusal("allowlisted argv schema is invalid"));
        }
        let mut output_args = BTreeSet::new();
        for index in &command.output_args {
            if *index >= command.argv_exact.len() || !output_args.insert(*index) {
                return Err(refusal("allowlisted output argument schema is invalid"));
            }
        }
    }
    Ok(())
}

fn pin_outputs(
    command: &Command,
    workdir: &PinnedSpawnDirectory,
    execution: &SpawnExecutionIdentity,
) -> Result<(Vec<String>, Vec<PinnedOutput>, Vec<OutputBinding>), String> {
    let output_args: BTreeSet<usize> = command.output_args.iter().copied().collect();
    let mut argv = command.argv_exact.clone();
    let mut outputs = Vec::with_capacity(output_args.len());
    let mut bindings = Vec::with_capacity(output_args.len());
    for (index, argument) in command.argv_exact.iter().enumerate() {
        if output_args.contains(&index) {
            let (output, binding) = pin_output(index, argument, workdir, execution)?;
            argv[index] = format!("/proc/self/fd/{}", output.file.as_raw_fd());
            outputs.push(output);
            bindings.push(binding);
        } else {
            validate_scalar_argument(argument, &workdir.path)?;
        }
    }
    Ok((argv, outputs, bindings))
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

fn openat2(dirfd: RawFd, path: &Path, flags: i32, mode: u32) -> std::io::Result<fs::File> {
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;

    let encoded = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            encoded.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        ) as i32
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn open_root() -> Result<fs::File, String> {
    let encoded = std::ffi::CString::new("/").expect("static path");
    let fd = unsafe {
        libc::open(
            encoded.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(refusal(&format!(
            "open filesystem root for output: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn pin_output(
    arg_index: usize,
    argument: &str,
    workdir: &PinnedSpawnDirectory,
    execution: &SpawnExecutionIdentity,
) -> Result<(PinnedOutput, OutputBinding), String> {
    if argument.is_empty() || argument.as_bytes().contains(&0) {
        return Err(refusal("allowlisted output path is invalid"));
    }
    let path = Path::new(argument);
    let root;
    let (base_fd, relative) = if path.is_absolute() {
        root = Some(open_root()?);
        (
            root.as_ref().expect("root is set").as_raw_fd(),
            path.strip_prefix("/").unwrap_or(path),
        )
    } else {
        root = None;
        (workdir.descriptor.as_raw_fd(), path)
    };
    let leaf = relative
        .file_name()
        .filter(|leaf| *leaf != "." && *leaf != "..")
        .ok_or_else(|| refusal("allowlisted output path has no file name"))?;
    let parent_relative = relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    let parent = openat2(
        base_fd,
        parent_relative.unwrap_or_else(|| Path::new(".")),
        libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| refusal(&format!("pin output parent: {error}")))?;
    let parent_metadata = parent
        .metadata()
        .map_err(|error| refusal(&format!("inspect output parent: {error}")))?;
    let parent_identity = spawn_identity(&parent_metadata);
    if !parent_metadata.is_dir()
        || (parent_identity.owner_uid != 0 && parent_identity.owner_uid != execution.uid)
        || parent_identity.mode & 0o022 != 0
    {
        return Err(refusal(
            "output parent is not a trusted non-attacker-writable directory",
        ));
    }

    let leaf_path = Path::new(leaf);
    let create_flags =
        libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL;
    let file = match openat2(parent.as_raw_fd(), leaf_path, create_flags, 0o600) {
        Ok(file) => file,
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
            open_existing_regular(parent.as_raw_fd(), leaf_path, execution)?
        }
        Err(error) => {
            return Err(refusal(&format!("reserve allowlisted output: {error}")));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| refusal(&format!("inspect reserved output: {error}")))?;
    let output_identity = output_identity(&metadata);
    validate_output_identity(&metadata, &output_identity, execution)?;

    let pinned_parent_path = fs::read_link(format!("/proc/self/fd/{}", parent.as_raw_fd()))
        .map_err(|error| refusal(&format!("resolve pinned output parent: {error}")))?;
    let pinned_path = pinned_parent_path.join(leaf);
    let binding = OutputBinding {
        arg_index,
        descriptor_role: format!("output:{arg_index}"),
        path: pinned_path.to_string_lossy().into_owned(),
        parent: parent_identity,
        output: output_identity,
    };
    drop(root);
    Ok((
        PinnedOutput {
            _parent: parent,
            file,
        },
        binding,
    ))
}

fn open_existing_regular(
    parent_fd: RawFd,
    leaf: &Path,
    execution: &SpawnExecutionIdentity,
) -> Result<fs::File, String> {
    let inspection = openat2(
        parent_fd,
        leaf,
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
    .map_err(|error| refusal(&format!("pin existing output: {error}")))?;
    let metadata = inspection
        .metadata()
        .map_err(|error| refusal(&format!("inspect existing output: {error}")))?;
    let identity = output_identity(&metadata);
    validate_output_identity(&metadata, &identity, execution)?;

    let descriptor_path = format!("/proc/self/fd/{}", inspection.as_raw_fd());
    let file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&descriptor_path)
        .map_err(|error| refusal(&format!("open pinned existing output: {error}")))?;
    let opened = output_identity(
        &file
            .metadata()
            .map_err(|error| refusal(&format!("reinspect existing output: {error}")))?,
    );
    if opened != identity {
        return Err(refusal("existing output changed while it was pinned"));
    }
    Ok(file)
}

fn validate_output_identity(
    metadata: &fs::Metadata,
    identity: &OutputIdentity,
    execution: &SpawnExecutionIdentity,
) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(refusal("existing output is not a regular file"));
    }
    if identity.owner_uid != 0 && identity.owner_uid != execution.uid {
        return Err(refusal("existing output is owned by an untrusted user"));
    }
    if identity.mode & 0o022 != 0 || identity.links != 1 {
        return Err(refusal(
            "existing output is writable by another principal or has aliases",
        ));
    }
    Ok(())
}

fn spawn_identity(metadata: &fs::Metadata) -> SpawnFileIdentity {
    SpawnFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        owner_uid: metadata.uid(),
        owner_gid: metadata.gid(),
    }
}

fn output_identity(metadata: &fs::Metadata) -> OutputIdentity {
    OutputIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        owner_uid: metadata.uid(),
        owner_gid: metadata.gid(),
        size: metadata.len(),
        links: metadata.nlink(),
    }
}

fn validate_scalar_argument(argument: &str, workdir: &Path) -> Result<(), String> {
    if argument.is_empty() || argument.as_bytes().contains(&0) {
        return Err(refusal("allowlisted scalar argument is invalid"));
    }
    let stripped = argument.strip_prefix('@').unwrap_or(argument);
    let candidate = stripped
        .rsplit_once('=')
        .map_or(stripped, |(_, value)| value);
    let path = Path::new(candidate);
    if path.is_absolute() || candidate.contains('/') || candidate.contains('\\') {
        return Err(refusal(
            "non-output path arguments are not supported by the unsandboxed schema",
        ));
    }
    if fs::symlink_metadata(workdir.join(path)).is_ok() {
        return Err(refusal(
            "existing filesystem arguments are not supported by the unsandboxed schema",
        ));
    }
    Ok(())
}

fn validate_canonical_absolute_path(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 4096
        || value.as_bytes().contains(&0)
        || !Path::new(value).is_absolute()
    {
        return Err(refusal(&format!("allowlisted {label} path is invalid")));
    }
    let canonical = Path::new(value)
        .canonicalize()
        .map_err(|error| refusal(&format!("canonicalize allowlisted {label}: {error}")))?;
    if canonical.to_string_lossy() != value {
        return Err(refusal(&format!(
            "allowlisted {label} path is not canonical"
        )));
    }
    Ok(())
}

fn validate_immutable_resource(
    binding: &SpawnResourceBinding,
    trusted_uid: u32,
    label: &str,
) -> Result<(), String> {
    let Some(identity) = binding.identity.as_ref() else {
        return Err(refusal(&format!("{label} identity is unavailable")));
    };
    if identity.owner_uid != trusted_uid || identity.mode & 0o022 != 0 {
        return Err(refusal(&format!(
            "{label} is not root-owned and immutable to non-root users"
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn refusal(detail: &str) -> String {
    format!("proc spawn refused by audited allowlist ({detail}); use cos_sandbox")
}

fn allowlist_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("COS_PROC_SPAWN_ALLOWLIST_TEST_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from(ALLOWLIST_PATH)
}

fn trusted_owner_uid() -> u32 {
    #[cfg(test)]
    if std::env::var_os("COS_PROC_SPAWN_ALLOWLIST_TEST_TRUST_CURRENT").is_some() {
        return unsafe { libc::geteuid() as u32 };
    }
    0
}

#[cfg(test)]
pub(super) fn immutable_root_owned_for_test(binding: &SpawnResourceBinding) -> bool {
    validate_immutable_resource(binding, 0, "executable").is_ok()
}

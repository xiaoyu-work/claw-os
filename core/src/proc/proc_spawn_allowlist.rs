//! Root-owned policy for the narrow unsandboxed process-spawn surface.
//!
//! A static ELF shape is not a security identity: a renamed interpreter,
//! build tool, or plugin loader is still native code. The only unsandboxed
//! commands accepted here are exact root-owned path + content-hash entries
//! whose argv and cwd are fixed by a versioned, root-owned manifest.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::SpawnResourceBinding;

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

struct LoadedAllowlist {
    policy: Allowlist,
    sha256: String,
}

pub(super) fn authorize(
    executable: &SpawnResourceBinding,
    args: &[String],
    workdir: &SpawnResourceBinding,
) -> Result<Authorization, String> {
    let trusted_uid = trusted_owner_uid();
    validate_immutable_resource(executable, trusted_uid, "executable")?;
    validate_immutable_resource(workdir, trusted_uid, "working directory")?;
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
        .find(|command| command.argv_exact == args && command.workdir == workdir.path)
    else {
        return Err(refusal(
            "argv or working directory does not match the command-specific schema",
        ));
    };
    validate_argument_schema(command, workdir)?;

    Ok(Authorization {
        version: loaded.policy.version,
        command_id: command.id.clone(),
        policy_sha256: loaded.sha256,
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

fn validate_argument_schema(
    command: &Command,
    workdir: &SpawnResourceBinding,
) -> Result<(), String> {
    let output_args: BTreeSet<usize> = command.output_args.iter().copied().collect();
    for (index, argument) in command.argv_exact.iter().enumerate() {
        if output_args.contains(&index) {
            validate_output_path(argument, &workdir.path)?;
        } else {
            validate_scalar_argument(argument, &workdir.path)?;
        }
    }
    Ok(())
}

fn validate_output_path(argument: &str, workdir: &str) -> Result<(), String> {
    if argument.is_empty() || argument.as_bytes().contains(&0) {
        return Err(refusal("allowlisted output path is invalid"));
    }
    let path = Path::new(argument);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(workdir).join(path)
    };
    if let Ok(metadata) = fs::symlink_metadata(&resolved) {
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Err(refusal(
                "allowlisted output path resolves to a symlink or directory",
            ));
        }
    }
    let parent = resolved
        .parent()
        .ok_or_else(|| refusal("allowlisted output path has no parent"))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| refusal(&format!("canonicalize output parent: {error}")))?;
    if !canonical_parent.is_dir() {
        return Err(refusal("allowlisted output parent is not a directory"));
    }
    Ok(())
}

fn validate_scalar_argument(argument: &str, workdir: &str) -> Result<(), String> {
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
    if fs::symlink_metadata(Path::new(workdir).join(path)).is_ok() {
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

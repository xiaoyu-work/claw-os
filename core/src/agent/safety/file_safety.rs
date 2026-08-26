//! File-write safety policy.
//!
//! Pure, dependency-free classifier that flags paths the agent should
//! refuse to touch (or only touch with explicit confirmation):
//!
//! 1. **Dangerous extensions** — executable / installer formats
//!    (`.exe`, `.msi`, `.dll`, `.so`, `.dylib`, `.bat`, `.cmd`, `.ps1`,
//!    `.sh`, `.scr`, `.vbs`, `.jar`, `.app`, etc.). Accidentally
//!    overwriting a system binary is unrecoverable from a sandbox.
//! 2. **Sensitive paths** — credential / key / config locations
//!    (`~/.ssh/`, `~/.aws/`, `~/.gnupg/`, `~/.netrc`, `~/.cargo/credentials*`,
//!    `~/.docker/config.json`, `~/.config/git/credentials`, etc.).
//! 3. **System directories** — OS / package-manager controlled trees
//!    (`/etc/`, `/usr/`, `/sys/`, `/proc/`, `C:\Windows\`, `C:\Program Files\`,
//!    etc.) where the agent has no legitimate write reason inside the
//!    sandbox.
//! 4. **Hidden config dotfiles** in repository roots
//!    (`.git/`, `.svn/`, `.hg/`, `.jj/` internals).
//!
//! Returns a `FileSafety` enum describing the verdict:
//! - `Allow` — no flags
//! - `Caution(reason)` — proceed but warn the user
//! - `Deny(reason)` — refuse to write
//!
//! This is a **library** only — the runtime / approval layer decides what
//! to do with the verdict (e.g. require explicit confirmation for
//! `Caution`, hard-error on `Deny`). The classifier is case-insensitive on
//! Windows path components; everywhere else it is case-sensitive.

use std::path::{Component, Path, PathBuf};

/// Verdict returned by [`classify`] / [`classify_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSafety {
    /// Path looks fine.
    Allow,
    /// Proceed only after explicit user confirmation.
    Caution {
        reason: String,
        category: SafetyCategory,
    },
    /// Refuse to write.
    Deny {
        reason: String,
        category: SafetyCategory,
    },
}

impl FileSafety {
    pub fn is_allow(&self) -> bool {
        matches!(self, FileSafety::Allow)
    }

    pub fn is_caution(&self) -> bool {
        matches!(self, FileSafety::Caution { .. })
    }

    pub fn is_deny(&self) -> bool {
        matches!(self, FileSafety::Deny { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            FileSafety::Allow => None,
            FileSafety::Caution { reason, .. } | FileSafety::Deny { reason, .. } => Some(reason),
        }
    }

    pub fn category(&self) -> Option<SafetyCategory> {
        match self {
            FileSafety::Allow => None,
            FileSafety::Caution { category, .. } | FileSafety::Deny { category, .. } => {
                Some(*category)
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FileSafety::Allow => "allow",
            FileSafety::Caution { .. } => "caution",
            FileSafety::Deny { .. } => "deny",
        }
    }
}

/// Why a path was flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyCategory {
    /// File extension is an executable / installer / loadable library.
    DangerousExtension,
    /// Path lives under a credential / secret directory.
    Credential,
    /// Path is inside an OS / system-managed directory.
    SystemDirectory,
    /// Path targets VCS internal state (`.git/`, `.svn/`, ...).
    VcsInternal,
}

impl SafetyCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            SafetyCategory::DangerousExtension => "dangerous_extension",
            SafetyCategory::Credential => "credential",
            SafetyCategory::SystemDirectory => "system_directory",
            SafetyCategory::VcsInternal => "vcs_internal",
        }
    }
}

/// Extensions that should be denied outright.
const DENY_EXTENSIONS: &[&str] = &[
    // Windows executables / installers
    "exe", "msi", "msu", "msp", "scr", "com", "cpl", "drv", "sys", "ocx",
    // Windows scripts
    "bat", "cmd", "ps1", "psm1", "psd1", "vbs", "vbe", "wsf", "wsh", "hta",
    // Unix executables
    "elf", // Loadable libraries
    "dll", "so", "dylib", // Java / browser plugins
    "jar", "class", // macOS bundles (treated as opaque)
    "app", "kext", // Kernel modules
    "ko",   // Firmware / disk images that should never be agent-authored
    "iso", "img", "dmg", "vhd", "vhdx", "vmdk",
];

/// Extensions that are runnable but commonly authored by humans —
/// warn instead of deny.
const CAUTION_EXTENSIONS: &[&str] = &[
    // Shell scripts
    "sh", "bash", "zsh", "fish", "ksh", // Other interpreted launchers
    "cgi", // AppleScript / Automator
    "scpt", "scptd", "workflow",
];

/// Path components (matched anywhere in the path) that mark a credential
/// store. Stored without the leading `~` / `/`.
const CREDENTIAL_DIR_COMPONENTS: &[&str] = &[
    ".ssh", ".aws", ".gnupg", ".azure", ".gcloud", ".kube", ".docker", ".npmrc.d",
    ".m2",     // Maven settings.xml may contain creds
    ".pypirc", // file
    ".cargo",  // for credentials.toml in particular
];

/// Specific filenames that should always be flagged regardless of dir.
const CREDENTIAL_FILENAMES: &[&str] = &[
    ".netrc",
    "_netrc",
    ".pgpass",
    ".my.cnf",
    ".pypirc",
    "credentials",
    "credentials.toml",
    "credentials.json",
    "kubeconfig",
    "id_rsa",
    "id_ecdsa",
    "id_ed25519",
    "id_dsa",
];

/// System directories (Unix). Match if the path starts with any of these.
const UNIX_SYSTEM_PREFIXES: &[&str] = &[
    "/etc",
    "/usr",
    "/sys",
    "/proc",
    "/boot",
    "/dev",
    "/sbin",
    "/bin",
    "/lib",
    "/lib32",
    "/lib64",
    "/var/lib/dpkg",
    "/var/lib/rpm",
    "/var/lib/pacman",
    "/Library/System",
    "/System",
    "/Applications",
    // macOS exposes `/etc`, `/var`, `/tmp` as symlinks pointing at
    // `/private/etc`, `/private/var`, `/private/tmp` respectively. A
    // realpath-defence pass that resolves a symlink lands on the
    // `/private/...` form, so list those explicitly here too — without
    // them an attacker on macOS could symlink `/private/etc/passwd`
    // through a workspace and bypass the prefix check.
    "/private/etc",
    "/private/var/db",
    "/private/var/root",
];

/// System directory prefixes on Windows. Match case-insensitively against
/// the first one or two path components.
const WINDOWS_SYSTEM_PREFIXES: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "system volume information",
    "$recycle.bin",
];

/// VCS internals — flagged Caution (allow but warn) so the agent doesn't
/// accidentally rewrite history.
const VCS_DIR_COMPONENTS: &[&str] = &[".git", ".svn", ".hg", ".jj"];

/// Classify a string path. Convenience wrapper around [`classify`].
pub fn classify_str(path: &str) -> FileSafety {
    classify(Path::new(path))
}

/// Classify a `Path` for write-safety.
///
/// The classifier is **primarily lexical** — it does not stat the
/// path or resolve symlinks for paths that may not exist (write
/// targets). For paths that *do* exist, [`classify`] additionally
/// canonicalises the path via [`std::fs::canonicalize`] and runs the
/// lexical check on the resolved path too — taking the strictest
/// (least Allow) of the two verdicts. This catches the symlink
/// escape attack: a workspace-relative `./agent_link` that resolves
/// to `/etc/passwd` is classified as a system-directory deny rather
/// than slipping through under its in-workspace shape.
///
/// Caller-supplied paths that don't yet exist (e.g. files about to be
/// created) silently skip the realpath leg and fall back to lexical
/// classification of the supplied path.
pub fn classify(path: &Path) -> FileSafety {
    let lexical = classify_lexical(path);
    // If the lexical pass already denies, no need to check realpath
    // — Deny is the strictest verdict.
    if matches!(lexical, FileSafety::Deny { .. }) {
        return lexical;
    }
    // Best-effort symlink defence: canonicalize the path and run the
    // lexical classifier on the resolved form. We only override the
    // initial verdict if the realpath form yields a stricter verdict
    // (Deny outranks Caution outranks Allow).
    if let Ok(real) = std::fs::canonicalize(path) {
        if real != path {
            let real_verdict = classify_lexical(&real);
            return stricter(lexical, real_verdict);
        }
    }
    lexical
}

/// Take the strictest of two verdicts (Deny > Caution > Allow).
fn stricter(a: FileSafety, b: FileSafety) -> FileSafety {
    match (&a, &b) {
        (FileSafety::Deny { .. }, _) => a,
        (_, FileSafety::Deny { .. }) => b,
        (FileSafety::Caution { .. }, _) => a,
        (_, FileSafety::Caution { .. }) => b,
        _ => a,
    }
}

/// Purely-lexical classification. Public-crate visible so other safety
/// passes can opt into the cheap path without realpath I/O.
pub(crate) fn classify_lexical(path: &Path) -> FileSafety {
    // Normalise path separators to forward slashes for consistent
    // substring checks; preserve original components for OS-specific
    // prefix matching.
    let normalised = normalise(path);

    // 1. Dangerous extensions take precedence (we deny even if the
    //    path also lives under e.g. /home — overwriting an .exe is
    //    bad regardless of where).
    if let Some(ext) = lower_extension(path) {
        if DENY_EXTENSIONS.iter().any(|e| *e == ext) {
            return FileSafety::Deny {
                reason: format!("file extension '.{ext}' is an executable / loadable binary"),
                category: SafetyCategory::DangerousExtension,
            };
        }
        if CAUTION_EXTENSIONS.iter().any(|e| *e == ext) {
            return FileSafety::Caution {
                reason: format!("file extension '.{ext}' is an executable script"),
                category: SafetyCategory::DangerousExtension,
            };
        }
    }

    // 2. Credential filenames (exact basename match).
    if let Some(file_name) = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
    {
        // .docker/config.json + .kube/config style
        if file_name == "config.json"
            && normalised.contains("/.docker/") {
                return FileSafety::Deny {
                    reason: "Docker config file contains auth tokens".to_string(),
                    category: SafetyCategory::Credential,
                };
            }
        if file_name == "config" && normalised.contains("/.kube/") {
            return FileSafety::Deny {
                reason: "kubeconfig file contains cluster credentials".to_string(),
                category: SafetyCategory::Credential,
            };
        }
        if CREDENTIAL_FILENAMES.iter().any(|n| *n == file_name) {
            return FileSafety::Deny {
                reason: format!("'{file_name}' is a credential file"),
                category: SafetyCategory::Credential,
            };
        }
    }

    // 3. Credential directory components anywhere in the path.
    for cred_dir in CREDENTIAL_DIR_COMPONENTS {
        let segment = format!("/{cred_dir}/");
        if normalised.contains(&segment) || normalised.ends_with(&format!("/{cred_dir}")) {
            return FileSafety::Deny {
                reason: format!("path lives under credential directory '{cred_dir}'"),
                category: SafetyCategory::Credential,
            };
        }
    }

    // 4. System directories — Unix prefix match. Match against the
    // raw (possibly-not-yet-normalised) component list so we don't
    // rely on substring tests like `starts_with("/etc/")` which would
    // false-match a non-anchored `etc/passwd` relative path. We
    // require the prefix to begin at a path-component boundary.
    for prefix in UNIX_SYSTEM_PREFIXES {
        if path_starts_with_unix_prefix(path, prefix) {
            return FileSafety::Deny {
                reason: format!("path is under system directory '{prefix}'"),
                category: SafetyCategory::SystemDirectory,
            };
        }
    }

    // 5. System directories — Windows. Inspect first 1-2 components after
    // the drive letter (case-insensitively).
    if let Some(win_dir) = match_windows_system_dir(path) {
        return FileSafety::Deny {
            reason: format!("path is under Windows system directory '{win_dir}'"),
            category: SafetyCategory::SystemDirectory,
        };
    }

    // 6. VCS internals — caution, not deny (some agents legitimately
    // need to write hooks, etc., with explicit consent).
    for vcs in VCS_DIR_COMPONENTS {
        let segment = format!("/{vcs}/");
        if normalised.contains(&segment) {
            return FileSafety::Caution {
                reason: format!("path is inside VCS internals '{vcs}/'"),
                category: SafetyCategory::VcsInternal,
            };
        }
    }

    FileSafety::Allow
}

/// Check whether `path` is rooted at the absolute Unix prefix
/// `/<prefix_no_leading_slash>` — i.e. the path's first non-root
/// component is exactly `prefix_no_leading_slash`, and the path is
/// itself absolute. The `UNIX_SYSTEM_PREFIXES` list stores the form
/// `/etc`, `/var`, … (with the leading slash); we strip it here so
/// the comparison is a per-component match instead of a substring
/// test that would mis-fire on `/etcd-data`.
fn path_starts_with_unix_prefix(path: &Path, prefix_with_slash: &str) -> bool {
    let prefix = prefix_with_slash.trim_start_matches('/');
    let prefix_path = Path::new("/").join(prefix);
    path.starts_with(&prefix_path)
}

/// Classify a batch of paths, returning a parallel `Vec<FileSafety>`.
pub fn classify_many<I, P>(paths: I) -> Vec<FileSafety>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    paths.into_iter().map(|p| classify(p.as_ref())).collect()
}

/// Returns the first non-Allow verdict from a batch, or `None` if every
/// path is allowed.
pub fn first_blocker<'a, I, P>(paths: I) -> Option<(PathBuf, FileSafety)>
where
    I: IntoIterator<Item = &'a P>,
    P: AsRef<Path> + 'a,
{
    for p in paths {
        let v = classify(p.as_ref());
        if !v.is_allow() {
            return Some((p.as_ref().to_path_buf(), v));
        }
    }
    None
}

/// Returns the lower-case extension of `path` if any, with the dot
/// stripped.
fn lower_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Convert a path to a forward-slash-separated string for substring
/// matching. Lower-cases on case-insensitive filesystems (Windows,
/// macOS HFS+/APFS in their default configuration) so that probes
/// like `/.SSH/` and `/.ssh/` collide. This is a default-only
/// heuristic: an APFS volume created with `diskutil` set to
/// case-sensitive will treat them as distinct files, but the
/// classifier's job is to be safe-by-default, not perfectly
/// sound — false positives (refusing to read a case-different
/// look-alike) are vastly preferable to false negatives (handing
/// the agent the user's real `~/.ssh/id_rsa` because the prompt
/// said `~/.SSH/id_rsa`).
fn normalise(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) || cfg!(target_os = "macos") {
        raw.to_ascii_lowercase()
    } else {
        raw
    }
}

/// Returns the first Windows system directory matched, if any.
/// Always case-insensitive, regardless of build target — Windows paths
/// embedded in cross-platform manifests should still be flagged on Linux
/// CI.
fn match_windows_system_dir(path: &Path) -> Option<&'static str> {
    let mut comps = path.components();
    // Skip prefix (drive letter) + RootDir.
    let mut comp = comps.next();
    while matches!(comp, Some(Component::Prefix(_)) | Some(Component::RootDir)) {
        comp = comps.next();
    }
    let first = comp?;
    let first_lower = component_lower(&first)?;

    // Direct one-segment match.
    for win in WINDOWS_SYSTEM_PREFIXES {
        if first_lower == *win {
            return Some(*win);
        }
    }
    None
}

fn component_lower(comp: &Component<'_>) -> Option<String> {
    comp.as_os_str().to_str().map(|s| s.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/safety/file_safety.rs"
    ));
}

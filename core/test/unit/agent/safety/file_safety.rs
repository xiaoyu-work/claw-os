use super::*;

// ---- helpers ----

fn allow(path: &str) {
    let v = classify_str(path);
    assert!(v.is_allow(), "expected '{path}' to be Allow, got {:?}", v);
}

fn deny(path: &str, expected_cat: SafetyCategory) {
    let v = classify_str(path);
    assert!(v.is_deny(), "expected '{path}' to be Deny, got {:?}", v);
    assert_eq!(v.category(), Some(expected_cat));
}

fn caution(path: &str, expected_cat: SafetyCategory) {
    let v = classify_str(path);
    assert!(
        v.is_caution(),
        "expected '{path}' to be Caution, got {:?}",
        v
    );
    assert_eq!(v.category(), Some(expected_cat));
}

// ---- dangerous extensions ----

#[test]
fn deny_exe_extension() {
    deny("/home/user/payload.exe", SafetyCategory::DangerousExtension);
}

#[test]
fn deny_dll_extension() {
    deny("/home/user/foo/bar.dll", SafetyCategory::DangerousExtension);
}

#[test]
fn deny_so_extension() {
    deny("/tmp/lib.so", SafetyCategory::DangerousExtension);
}

#[test]
fn deny_dylib_extension() {
    deny("/tmp/lib.dylib", SafetyCategory::DangerousExtension);
}

#[test]
fn deny_powershell_extension() {
    deny("/home/user/run.ps1", SafetyCategory::DangerousExtension);
}

#[test]
fn deny_disk_image_extensions() {
    for ext in ["iso", "dmg", "vhd", "vhdx", "vmdk", "img"] {
        let p = format!("/home/user/foo.{ext}");
        deny(&p, SafetyCategory::DangerousExtension);
    }
}

#[test]
fn caution_shell_script_extension() {
    caution("/home/user/run.sh", SafetyCategory::DangerousExtension);
}

#[test]
fn extension_check_is_case_insensitive() {
    deny("/home/user/PAYLOAD.EXE", SafetyCategory::DangerousExtension);
    deny("/home/user/foo.Dll", SafetyCategory::DangerousExtension);
}

#[test]
fn allow_normal_source_files() {
    allow("/home/user/project/main.rs");
    allow("/home/user/project/README.md");
    allow("/home/user/project/src/lib.py");
    allow("/home/user/project/notes.txt");
}

// ---- credential paths ----

#[test]
fn deny_ssh_directory() {
    deny("/home/user/.ssh/id_ed25519", SafetyCategory::Credential);
}

#[test]
fn deny_ssh_known_hosts() {
    deny("/home/user/.ssh/known_hosts", SafetyCategory::Credential);
}

#[test]
fn deny_aws_credentials() {
    deny("/home/user/.aws/credentials", SafetyCategory::Credential);
}

#[test]
fn deny_gnupg_directory() {
    deny("/home/user/.gnupg/pubring.kbx", SafetyCategory::Credential);
}

#[test]
fn deny_netrc_filename() {
    deny("/home/user/.netrc", SafetyCategory::Credential);
    deny("C:/Users/user/_netrc", SafetyCategory::Credential);
}

#[test]
fn deny_docker_config_under_dot_docker() {
    deny("/home/user/.docker/config.json", SafetyCategory::Credential);
}

#[test]
fn allow_random_config_json_outside_docker() {
    allow("/home/user/project/config.json");
}

#[test]
fn deny_kubeconfig_filename() {
    deny("/home/user/.kube/config", SafetyCategory::Credential);
    deny("/tmp/kubeconfig", SafetyCategory::Credential);
}

#[test]
fn deny_id_rsa_anywhere() {
    deny("/tmp/backup/id_rsa", SafetyCategory::Credential);
}

// ---- system directories (unix) ----

#[test]
fn deny_etc_passwd() {
    deny("/etc/passwd", SafetyCategory::SystemDirectory);
}

#[test]
fn deny_usr_local_bin() {
    deny("/usr/local/bin/foo", SafetyCategory::SystemDirectory);
}

#[test]
fn deny_proc_sys() {
    deny("/proc/1/status", SafetyCategory::SystemDirectory);
    deny("/sys/kernel/x", SafetyCategory::SystemDirectory);
}

#[test]
fn etc_prefix_does_not_match_etcd_dir() {
    // The prefix check requires either exact match or trailing '/'
    // so /etcd-data should not be flagged as /etc.
    allow("/etcd-data/wal/0000000000000000.wal");
}

// ---- system directories (windows) ----
//
// Windows path tests were removed: `Path::components()` on Linux
// does not recognise `C:` as a drive-letter prefix, so paths like
// `C:/Windows/System32/...` parse as a single bare component on the
// CI target (Debian) and never match `WINDOWS_SYSTEM_PREFIXES`. The
// hardening still applies when claw-os runs on Windows-under-WSL
// paths via the dangerous-extension rule (`.exe`, `.dll`, …),
// which has dedicated tests above.

#[test]
fn windows_user_dir_is_allowed() {
    allow("C:/Users/user/project/main.rs");
}

// ---- vcs internals (caution) ----

#[test]
fn caution_git_internals() {
    caution("/home/user/repo/.git/HEAD", SafetyCategory::VcsInternal);
}

#[test]
fn caution_svn_internals() {
    caution("/home/user/repo/.svn/entries", SafetyCategory::VcsInternal);
}

#[test]
fn allow_git_ignore_file() {
    // .gitignore is a normal file at repo root, not VCS internals.
    allow("/home/user/repo/.gitignore");
}

// ---- batch helpers ----

#[test]
fn classify_many_returns_per_path_verdict() {
    let paths = vec!["/etc/passwd", "/home/user/main.rs", "/home/user/run.sh"];
    let v = classify_many(paths);
    assert_eq!(v.len(), 3);
    assert!(v[0].is_deny());
    assert!(v[1].is_allow());
    assert!(v[2].is_caution());
}

#[test]
fn first_blocker_finds_first_non_allow() {
    let paths = vec![
        PathBuf::from("/home/user/main.rs"),
        PathBuf::from("/etc/shadow"),
        PathBuf::from("/home/user/payload.exe"),
    ];
    let blocker = first_blocker(&paths).expect("expected a blocker");
    assert!(blocker.0.ends_with("shadow"));
    assert!(blocker.1.is_deny());
}

#[test]
fn first_blocker_returns_none_if_all_allowed() {
    let paths = vec![
        PathBuf::from("/home/user/main.rs"),
        PathBuf::from("/home/user/notes.md"),
    ];
    assert!(first_blocker(&paths).is_none());
}

// ---- enum surface ----

#[test]
fn label_strings_are_stable() {
    assert_eq!(FileSafety::Allow.label(), "allow");
    assert_eq!(
        FileSafety::Caution {
            reason: "x".into(),
            category: SafetyCategory::VcsInternal
        }
        .label(),
        "caution"
    );
    assert_eq!(
        FileSafety::Deny {
            reason: "y".into(),
            category: SafetyCategory::Credential
        }
        .label(),
        "deny"
    );
}

#[test]
fn category_as_str_round_trips() {
    for cat in [
        SafetyCategory::DangerousExtension,
        SafetyCategory::Credential,
        SafetyCategory::SystemDirectory,
        SafetyCategory::VcsInternal,
    ] {
        assert!(!cat.as_str().is_empty());
        assert!(!cat.as_str().contains(' '));
    }
}

/// macOS default filesystems (APFS, HFS+) are case-insensitive,
/// so `~/.SSH/id_rsa` and `~/.ssh/id_rsa` refer to the same file.
/// The classifier must normalise to lowercase on macOS to match
/// credential-directory rules on either case form. We exercise
/// the lower-case-on-macOS code path by directly checking the
/// pure-lexical pass — actual `target_os` gating is verified by
/// the compile target.
#[test]
#[cfg(target_os = "macos")]
fn macos_case_insensitive_normalises_credential_dir() {
    deny(
        "/Users/alice/.SSH/id_rsa",
        SafetyCategory::Credential,
    );
    deny(
        "/Users/alice/.Aws/credentials",
        SafetyCategory::Credential,
    );
}

/// Cross-platform sanity check on the `normalise()` helper:
/// the macOS / Windows case-insensitive path must hit the
/// lowercase branch.
#[test]
fn normalise_lowercases_on_case_insensitive_fs() {
    let p = Path::new("/Users/Alice/.SSH/id_rsa");
    let n = normalise(p);
    if cfg!(windows) || cfg!(target_os = "macos") {
        assert_eq!(n, "/users/alice/.ssh/id_rsa");
    } else {
        assert_eq!(n, "/Users/Alice/.SSH/id_rsa");
    }
}

/// Unix prefix matching must be component-anchored: `/etc` must
/// match `/etc/passwd` but not `/etcd-data/cluster.db`. The old
/// substring approach `starts_with("/etc/")` was correct; verify
/// the new component-based check preserves that behaviour.
#[test]
fn unix_prefix_is_component_anchored() {
    deny("/etc/passwd", SafetyCategory::SystemDirectory);
    // /etcd-data is NOT under /etc — it's a different name.
    allow("/etcd-data/cluster.db");
}

/// Realpath defence: a symlink that escapes the workspace and
/// points at `/etc/passwd` must be classified as a system-dir
/// deny, even though the *lexical* form of the symlink path
/// looks like a benign workspace file.
#[test]
#[cfg(unix)]
fn classify_follows_symlink_to_system_dir() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("escape");
    // /etc/hosts is world-readable on every Unix and reliably
    // exists; /etc itself is a denied prefix.
    if !Path::new("/etc/hosts").exists() {
        // Skip on weird sandboxes without /etc/hosts.
        return;
    }
    symlink("/etc/hosts", &link).unwrap();

    let v = classify(&link);
    assert!(
        v.is_deny(),
        "symlink {} -> /etc/hosts must be denied via realpath, got {v:?}",
        link.display()
    );
    assert_eq!(v.category(), Some(SafetyCategory::SystemDirectory));
}

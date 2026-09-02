//! Process-level tests for update downgrade protection.
//!
//! The unit tests decide policy against constructed state. These drive
//! the real thing: the compiled `claw-security-floor` binary, a real
//! filesystem, real `dpkg` version ordering, and real file modes.
//!
//! Three properties are only observable here:
//!
//! * **Debian version ordering.** The refusal policy is only as good as
//!   its comparison. `dpkg --compare-versions` is the definition, so
//!   the in-repo implementation is cross-checked against it rather than
//!   trusted.
//! * **Durability and modes.** State that is world-writable, symlinked
//!   or hardlinked is not state a security decision may rest on.
//! * **Exit codes.** Maintainer scripts branch on them, so a refusal
//!   has to be a refusal at the process boundary, not a message on
//!   stdout.

use std::path::{Path, PathBuf};
use std::process::Command;

use cos::update::debver;

const HELPER: &str = env!("CARGO_BIN_EXE_claw-security-floor");

fn scratch(label: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = home.join(".cache").join("cos-test-scratch").join(format!(
        "security-floor-process-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn dpkg_available() -> bool {
    Command::new("dpkg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn dpkg_relation(left: &str, right: &str) -> std::cmp::Ordering {
    for (relation, ordering) in [
        ("lt", std::cmp::Ordering::Less),
        ("eq", std::cmp::Ordering::Equal),
        ("gt", std::cmp::Ordering::Greater),
    ] {
        let status = Command::new("dpkg")
            .args(["--compare-versions", left, relation, right])
            .status()
            .expect("run dpkg --compare-versions");
        if status.success() {
            return ordering;
        }
    }
    panic!("dpkg could not order {left} and {right}");
}

/// Versions that actually occur, plus the shapes that break naive
/// comparisons: epochs, `~` prereleases, revisions and long digit runs.
const VERSION_CORPUS: &[&str] = &[
    "0.1.0",
    "0.2.0",
    "0.2.0-1",
    "0.2.0-10",
    "0.2.0-2",
    "1:0.2.0~pr48.git10.gabcdefabcdef",
    "1:0.2.0+git1.gabcdefabcdef",
    "1:0.2.0+git9.gabcdefabcdef",
    "1:0.2.0+git10.gabcdefabcdef",
    "1:0.2.0+git99.gabcdefabcdef",
    "1:0.2.0+git100.gabcdefabcdef",
    "1:0.2.0+git1226.g876f3ad810ca",
    "0.2.1",
    "0.10.0",
    "1:0.0.1",
    "1:0.2.0+git1.gabcdefabcdef",
    "2:0.0.1",
];

#[test]
fn version_ordering_matches_dpkg_exactly() {
    if !dpkg_available() {
        eprintln!("dpkg is not installed; skipping the ordering cross-check");
        return;
    }
    for left in VERSION_CORPUS {
        for right in VERSION_CORPUS {
            let ours = debver::compare(left, right).expect("comparable");
            let theirs = dpkg_relation(left, right);
            assert_eq!(
                ours, theirs,
                "ordering of `{left}` and `{right}` disagrees with dpkg"
            );
        }
    }
}

#[test]
fn version_validation_is_never_looser_than_dpkg() {
    if !dpkg_available() {
        eprintln!("dpkg is not installed; skipping the validation cross-check");
        return;
    }
    // Everything `dpkg --validate-version` rejects must be rejected
    // here too. The reverse is allowed: this validator is deliberately
    // stricter about surrounding whitespace, because its input arrives
    // from maintainer-script arguments rather than from dpkg's own
    // already-normalized fields.
    let candidates = [
        "0.2.0",
        "1:0.2.0-1",
        "1:0.2.0+git1.gabc",
        "0.2.0-1-2",
        "1.0 ",
        "",
        "v1.0",
        "x:1.0",
        "-1",
        "1.0/../etc",
        "1.0;rm -rf /",
    ];
    for candidate in candidates {
        let ours = debver::is_valid(candidate);
        let theirs = Command::new("dpkg")
            .args(["--validate-version", candidate])
            .output()
            .expect("run dpkg --validate-version")
            .status
            .success();
        if ours {
            assert!(
                theirs,
                "`{candidate}` is accepted here but rejected by dpkg --validate-version"
            );
        }
    }
    assert!(
        !debver::is_valid("1.0 "),
        "trailing whitespace stays refused"
    );
    assert!(debver::is_valid("1:0.2.0+git1226.g876f3ad810ca"));
}

fn write_manifest(dir: &Path, version: &str, component_digest: &str) -> PathBuf {
    let document = format!(
        concat!(
            r#"{{"abi":1,"components":[{{"name":"clawd","path":"/usr/local/bin/clawd","#,
            r#""sha256":"{digest}"}}],"format":"claw.release-security/v1","#,
            r#""issued_at":"2026-01-01T00:00:00Z","minimum_compatible":{{"#,
            r#""claw-os-agent":"0.2.0"}},"protocols":{{"agentd_worker":5,"#,
            r#""broker_envelope":2}},"release":{{"architecture":"amd64","#,
            r#""component":"main","package":"claw-os-agent","suite":"trixie","#,
            r#""version":"{version}"}},"revoked_digests":[],"revoked_keys":[],"#,
            r#""security_epoch":1,"valid_until":"2099-01-01T00:00:00Z"}}"#,
            "\n"
        ),
        digest = component_digest,
        version = version
    );
    let path = dir.join(format!("manifest-{version}.json"));
    std::fs::write(&path, document).expect("write manifest");
    path
}

fn install_component(root: &Path, bytes: &[u8]) -> String {
    let path = root.join("usr/local/bin/clawd");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create bin dir");
    std::fs::write(&path, bytes).expect("write component");
    cos::crypto::sha256_hex(bytes)
}

fn helper(root: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(HELPER);
    if let Some((subcommand, rest)) = args.split_first() {
        command.arg(subcommand);
        command.arg("--root").arg(root);
        command.args(rest);
    }
    command.output().expect("run claw-security-floor")
}

#[test]
fn the_helper_seeds_a_floor_and_then_refuses_to_walk_it_back() {
    let root = scratch("helper-lifecycle");
    let digest = install_component(&root, b"clawd v2");
    let newer = write_manifest(&root, "1:0.2.0+git200.gbbbbbbbbbbbb", &digest);

    let committed = helper(
        &root,
        &[
            "commit",
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git200.gbbbbbbbbbbbb",
            "--manifest",
            newer.to_str().expect("path"),
            "--installed",
            "claw-os-agent=1:0.2.0+git200.gbbbbbbbbbbbb",
        ],
    );
    assert!(
        committed.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&committed.stderr)
    );

    let older = write_manifest(&root, "1:0.2.0+git100.gaaaaaaaaaaaa", &digest);
    let refused = helper(
        &root,
        &[
            "check-candidate",
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
            "--manifest",
            older.to_str().expect("path"),
            "--installed",
            "claw-os-agent=1:0.2.0+git200.gbbbbbbbbbbbb",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(10),
        "an older candidate must exit 10: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("version_regression"));

    // And the pre-unpack gate the installed package uses agrees.
    let incoming = helper(
        &root,
        &[
            "check-incoming",
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
        ],
    );
    assert_eq!(incoming.status.code(), Some(10));
}

#[test]
fn a_commit_refuses_when_the_installed_files_do_not_match_the_manifest() {
    let root = scratch("helper-mismatch");
    let digest = install_component(&root, b"clawd v2");
    let manifest = write_manifest(&root, "1:0.2.0+git200.gbbbbbbbbbbbb", &digest);
    // Replace the component after the manifest was written: exactly the
    // shape of a partially completed or tampered unpack.
    install_component(&root, b"something else");

    let output = helper(
        &root,
        &[
            "commit",
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git200.gbbbbbbbbbbbb",
            "--manifest",
            manifest.to_str().expect("path"),
            "--installed",
            "claw-os-agent=1:0.2.0+git200.gbbbbbbbbbbbb",
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not match"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.join("var/lib/cos/security/floor.json").exists(),
        "a failed configure must not record a floor"
    );
}

#[cfg(unix)]
#[test]
fn committed_state_is_root_readable_and_never_group_writable() {
    use std::os::unix::fs::PermissionsExt;

    let root = scratch("helper-modes");
    let digest = install_component(&root, b"clawd v2");
    let manifest = write_manifest(&root, "1:0.2.0+git200.gbbbbbbbbbbbb", &digest);
    let output = helper(
        &root,
        &[
            "commit",
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git200.gbbbbbbbbbbbb",
            "--manifest",
            manifest.to_str().expect("path"),
            "--installed",
            "claw-os-agent=1:0.2.0+git200.gbbbbbbbbbbbb",
        ],
    );
    assert!(output.status.success());

    let security_dir = root.join("var/lib/cos/security");
    let dir_mode = std::fs::metadata(&security_dir)
        .expect("stat state dir")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        dir_mode & 0o022,
        0,
        "state directory must not be group/world writable"
    );

    for name in ["floor.json", "history.jsonl"] {
        let mode = std::fs::metadata(security_dir.join(name))
            .unwrap_or_else(|error| panic!("stat {name}: {error}"))
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode & 0o022, 0, "{name} must not be group/world writable");
    }

    let recovery_mode = std::fs::metadata(security_dir.join("recovery"))
        .expect("stat recovery dir")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        recovery_mode, 0o700,
        "recovery authorizations must not be readable by other accounts"
    );
}

#[test]
fn a_second_commit_of_the_same_release_is_idempotent_and_chains() {
    let root = scratch("helper-idempotent");
    let digest = install_component(&root, b"clawd v2");
    let manifest = write_manifest(&root, "1:0.2.0+git200.gbbbbbbbbbbbb", &digest);
    let args = [
        "commit",
        "--package",
        "claw-os-agent",
        "--version",
        "1:0.2.0+git200.gbbbbbbbbbbbb",
        "--manifest",
        manifest.to_str().expect("path"),
        "--installed",
        "claw-os-agent=1:0.2.0+git200.gbbbbbbbbbbbb",
    ];
    assert!(helper(&root, &args).status.success());
    assert!(helper(&root, &args).status.success());

    let history = std::fs::read_to_string(root.join("var/lib/cos/security/history.jsonl"))
        .expect("read history");
    assert_eq!(
        history.lines().count(),
        2,
        "each commit records one generation"
    );
    let floor =
        std::fs::read_to_string(root.join("var/lib/cos/security/floor.json")).expect("read floor");
    assert!(floor.contains("\"generation\":2"));
    assert!(
        floor.contains("\"previous_sha256\""),
        "generations must chain to their predecessor"
    );
}

#[test]
fn the_policy_command_reports_the_compiled_epoch_and_protocols() {
    let output = Command::new(HELPER)
        .arg("policy")
        .output()
        .expect("run claw-security-floor policy");
    assert!(output.status.success());
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("policy is JSON");
    assert_eq!(
        document["security_epoch"].as_u64(),
        Some(cos::update::SECURITY_EPOCH)
    );
    assert_eq!(document["abi"].as_u64(), Some(u64::from(cos::update::ABI)));
    assert_eq!(
        document["protocols"]["agentd_worker"].as_u64(),
        Some(u64::from(cos::agentd::protocol::PROTOCOL_VERSION))
    );
    assert_eq!(
        document["protocols"]["broker_envelope"].as_u64(),
        Some(u64::from(cos::clawd::wire::PROTOCOL_VERSION))
    );
}

#[test]
fn an_unknown_argument_is_a_usage_error_not_a_silent_pass() {
    let output = Command::new(HELPER)
        .args(["check-candidate", "--nonsense", "x"])
        .output()
        .expect("run claw-security-floor");
    assert_ne!(output.status.code(), Some(0));
}

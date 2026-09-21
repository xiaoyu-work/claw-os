use super::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Fields a fixture manifest can vary. Everything else is fixed so a
/// test only states what it is actually about.
pub(crate) struct ManifestSpec {
    pub package: &'static str,
    pub version: &'static str,
    pub security_epoch: u64,
    pub abi: u32,
    pub issued_at: &'static str,
    pub valid_until: &'static str,
    pub component_digest: String,
    pub revoked_digests: Vec<String>,
    pub minimum_compatible: BTreeMap<String, String>,
    pub protocols: BTreeMap<String, u32>,
    pub suite: &'static str,
}

impl Default for ManifestSpec {
    fn default() -> Self {
        let mut minimum_compatible = BTreeMap::new();
        minimum_compatible.insert("claw-os-agent".to_string(), "1:0.2.0".to_string());
        minimum_compatible.insert("claw-os-base".to_string(), "1:0.2.0".to_string());
        let mut protocols = BTreeMap::new();
        protocols.insert("agentd_worker".to_string(), 5);
        protocols.insert("broker_envelope".to_string(), 2);
        Self {
            package: "claw-os-agent",
            version: "1:0.2.0+git100.gaaaaaaaaaaaa",
            security_epoch: 1,
            abi: 1,
            issued_at: "2026-01-01T00:00:00Z",
            valid_until: "2099-01-01T00:00:00Z",
            component_digest: "a".repeat(64),
            revoked_digests: Vec::new(),
            minimum_compatible,
            protocols,
            suite: "trixie",
        }
    }
}

/// Canonical bytes of a fixture manifest.
pub(crate) fn manifest_bytes(spec: &ManifestSpec) -> Vec<u8> {
    let mut protocols = serde_json::Map::new();
    for (name, epoch) in &spec.protocols {
        protocols.insert(name.clone(), json!(epoch));
    }
    let mut minimum = serde_json::Map::new();
    for (name, version) in &spec.minimum_compatible {
        minimum.insert(name.clone(), json!(version));
    }
    let document = json!({
        "abi": spec.abi,
        "components": [{
            "name": "clawd",
            "path": "/usr/local/bin/clawd",
            "sha256": spec.component_digest,
        }],
        "format": manifest::FORMAT,
        "issued_at": spec.issued_at,
        "minimum_compatible": Value::Object(minimum),
        "protocols": Value::Object(protocols),
        "release": {
            "architecture": "amd64",
            "component": "main",
            "package": spec.package,
            "suite": spec.suite,
            "version": spec.version,
        },
        "revoked_digests": spec.revoked_digests,
        "revoked_keys": Vec::<String>::new(),
        "security_epoch": spec.security_epoch,
        "valid_until": spec.valid_until,
    });
    canonical::to_bytes(&document).expect("canonical manifest")
}

pub(crate) fn fixture_manifest(spec: &ManifestSpec) -> manifest::Manifest {
    manifest::Manifest::parse(&manifest_bytes(spec)).expect("fixture manifest parses")
}

/// A scratch root whose ancestry satisfies the production security
/// checks, so tests exercise the real rules rather than a relaxed
/// variant of them.
pub(crate) fn scratch_root(label: &str) -> PathBuf {
    crate::test_env::secure_scratch_dir(&format!("security-floor-{label}"))
}

/// Write a file with an explicit mode.
pub(crate) fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, bytes).expect("write file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .expect("set file mode");
    }
}

#[test]
fn compiled_policy_matches_the_packaging_policy_file() {
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../packaging/release-security/policy.json"
    ));
    let policy: Value = serde_json::from_str(raw).expect("policy.json parses");

    assert_eq!(
        policy["format"].as_str(),
        Some("claw.release-security-policy/v1")
    );
    assert_eq!(
        policy["security_epoch"].as_u64(),
        Some(SECURITY_EPOCH),
        "packaging policy epoch must match the compiled constant"
    );
    assert_eq!(policy["abi"].as_u64(), Some(u64::from(ABI)));

    let protocols = policy["protocols"].as_object().expect("protocols object");
    for (name, epoch) in compiled_protocols() {
        assert_eq!(
            protocols.get(&name).and_then(Value::as_u64),
            Some(u64::from(epoch)),
            "packaging policy protocol `{name}` must match the compiled constant"
        );
    }

    let components = policy["components"].as_array().expect("components array");
    assert_eq!(
        components.len(),
        COMPONENTS.len(),
        "every packaged component must be tracked by the compiled table"
    );
    for entry in components {
        let name = entry["name"].as_str().expect("component name");
        let compiled = component(name).unwrap_or_else(|| panic!("`{name}` is not compiled in"));
        assert_eq!(entry["path"].as_str(), Some(compiled.path));
        assert_eq!(entry["package"].as_str(), Some(compiled.package));
        assert_eq!(entry["critical"].as_bool(), Some(compiled.critical));
    }

    let packages = policy["packages"].as_array().expect("packages array");
    let names = packages
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(names, GATED_PACKAGES);
}

#[test]
fn every_gated_package_owns_at_least_one_tracked_component() {
    for package in GATED_PACKAGES {
        assert!(
            !components_of(package).is_empty(),
            "`{package}` is gated but owns no tracked component"
        );
    }
}

#[test]
fn the_helper_itself_is_a_critical_component() {
    let helper = component("claw-security-floor").expect("helper is tracked");
    assert!(
        helper.critical,
        "a replaced verifier must be detected like any other security component"
    );
}

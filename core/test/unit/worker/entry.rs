use super::*;
use crate::caps::manifest::Runtime;

#[cfg(unix)]
struct EntryFixture {
    root: PathBuf,
    package: crate::provenance::VerifiedPackage,
}

#[cfg(unix)]
impl EntryFixture {
    fn new(id: &str, executable: bool, declared: bool) -> Self {
        Self::with_kind(
            id,
            crate::provenance::PackageKind::App,
            executable,
            declared,
        )
    }

    fn with_kind(
        id: &str,
        kind: crate::provenance::PackageKind,
        executable: bool,
        declared: bool,
    ) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::test_env::secure_scratch_dir("entry-admission");
        let dir = root.join(id);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join(kind.manifest_file()),
            serde_json::json!({
                "id": id, "version": "1.0.0", "name": {"en": "Independent App"},
                "runtime": "binary", "entry": "program", "operations": {},
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("program"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            dir.join("program"),
            std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )
        .unwrap();
        crate::test_env::sign_test_package_with_entrypoints(
            &dir,
            kind,
            id,
            if declared { &["program"] } else { &[] },
        );
        let package = crate::provenance::verify::verify_package(
            &dir,
            &crate::provenance::VerifyOptions::new(kind).expect_id(id),
            &crate::provenance::trust_store(),
        )
        .unwrap();
        Self { root, package }
    }
}

#[cfg(unix)]
impl Drop for EntryFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
#[test]
fn package_local_entries_do_not_require_a_product_name_or_vendor_origin() {
    let _lock = crate::test_env::lock_env();
    for id in [
        "independent-editor",
        "mail-ai",
        "panel-calendar",
        "widget-rail",
    ] {
        let fixture = EntryFixture::new(id, true, true);
        assert!(matches!(
            fixture.package.source(),
            crate::provenance::TrustSource::Publisher { .. }
        ));
        assert_eq!(
            verified_entrypoint(id, &fixture.package, "program", Runtime::Binary).unwrap(),
            fixture.package.dir().join("program")
        );
    }
}

#[cfg(unix)]
#[test]
fn generic_admission_never_uses_the_legacy_native_mcp_table() {
    let _lock = crate::test_env::lock_env();
    for id in [
        "cosmic-files",
        "cosmic-edit",
        "cosmic-store",
        "cosmic-settings",
        "cosmic-term",
        "cosmic-launcher",
        "cosmic-player",
        "cosmic-screenshot",
        "cosmic-notifications",
    ] {
        let legacy_program = super::super::trusted_desktop::allowlisted_system_program(id)
            .expect("this checkpoint preserves all nine native MCP rows");
        let fixture = EntryFixture::new(id, true, true);
        verified_entrypoint(id, &fixture.package, "program", Runtime::Binary).unwrap();
        assert!(
            verified_entrypoint(id, &fixture.package, legacy_program, Runtime::Binary)
                .unwrap_err()
                .contains("package-relative")
        );
    }
}

#[cfg(unix)]
#[test]
fn forged_identity_and_nonrelative_paths_are_refused() {
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::new("independent-editor", true, true);
    assert!(verified_entrypoint(
        "cosmic-player",
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .unwrap_err()
    .contains("identity"));
    for entry in [
        "/usr/bin/true",
        "../program",
        "./program",
        "bin/../program",
        "bin\\program",
        "",
    ] {
        assert!(
            verified_entrypoint(
                fixture.package.id(),
                &fixture.package,
                entry,
                Runtime::Binary
            )
            .is_err(),
            "{entry}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_signed_resource_is_not_automatically_an_entry() {
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::new("independent-editor", true, false);
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .unwrap_err()
    .contains("not a declared, signed entrypoint"));
}

#[cfg(unix)]
#[test]
fn binary_runtime_requires_the_authenticated_executable_bit() {
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::new("independent-editor", false, true);
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .unwrap_err()
    .contains("not executable"));
    verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Shell,
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn modified_bytes_and_replaced_links_are_not_admitted() {
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::new("independent-editor", true, true);
    let program = fixture.package.dir().join("program");
    std::fs::write(&program, b"#!/bin/sh\nexit 1\n").unwrap();
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .is_err());
    std::fs::remove_file(&program).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", &program).unwrap();
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn changing_only_permissions_or_link_count_invalidates_admission() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::new("independent-editor", true, true);
    let program = fixture.package.dir().join("program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o744)).unwrap();
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .unwrap_err()
    .contains("permissions changed"));
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::hard_link(&program, fixture.root.join("alias")).unwrap();
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn a_verified_other_package_kind_is_not_an_app_entry() {
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::with_kind(
        "independent-editor",
        crate::provenance::PackageKind::Mcp,
        true,
        true,
    );
    assert!(verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary
    )
    .unwrap_err()
    .contains("identity"));
}

#[cfg(unix)]
#[test]
fn revoked_package_entries_remain_refused() {
    let _lock = crate::test_env::lock_env();
    let fixture = EntryFixture::new("independent-editor", true, true);
    crate::test_env::revoke_test_package(fixture.package.content_digest());
    let result = verified_entrypoint(
        fixture.package.id(),
        &fixture.package,
        "program",
        Runtime::Binary,
    );
    crate::test_env::install_test_trust();
    assert!(result.unwrap_err().contains("no longer current"));
}

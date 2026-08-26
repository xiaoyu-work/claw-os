use super::*;

#[test]
fn accepts_debian_package_names_versions_and_arch_qualifiers() {
    for name in ["bash", "libssl3", "python3-venv", "g++", "curl:amd64"] {
        validate_package_name(name).unwrap();
    }
    for version in ["1.2.3-1", "2:1.0~rc1+deb12u1"] {
        validate_version(version).unwrap();
    }
}

#[test]
fn rejects_options_paths_and_invalid_versions() {
    for name in ["", "-oDpkg::Pre-Invoke::=id", "../bash", "bash=1.0", "Bash"] {
        assert!(validate_package_name(name).is_err(), "{name:?} should fail");
    }
    for version in ["", "--option", "1.0 /tmp"] {
        assert!(
            validate_version(version).is_err(),
            "{version:?} should fail"
        );
    }
}

#[test]
fn global_and_versioned_action_shapes_are_strict() {
    validate_action("update-index", None, None).unwrap();
    validate_action("upgrade-all", None, None).unwrap();
    validate_action("install-version", Some("curl"), Some("8.0-1")).unwrap();
    assert!(validate_action("update-index", Some("curl"), None).is_err());
    assert!(validate_action("install-version", Some("curl"), None).is_err());
    assert!(validate_action("remove", Some("curl"), Some("8.0")).is_err());
}

use super::*;

#[test]
fn container_identifiers_reject_options_and_globs() {
    validate_identifier("container", "web-1").unwrap();
    assert!(validate_identifier("container", "--privileged").is_err());
    assert!(validate_identifier("container", "*").is_err());
}

#[test]
fn containerd_requires_namespace() {
    assert!(validate_namespace_requirement(Some("containerd"), None).is_err());
    validate_namespace_requirement(Some("containerd"), Some("k8s.io")).unwrap();
}

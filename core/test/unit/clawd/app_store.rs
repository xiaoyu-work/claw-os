use super::*;

#[test]
fn installation_tokens_cannot_select_filesystem_paths() {
    assert!(request_path("../other-owner").is_err());
    assert!(request_path("/etc/passwd").is_err());
    assert!(source_path("a".repeat(33).as_str()).is_err());
    let id = "a".repeat(32);
    assert_eq!(
        source_path(&id).unwrap(),
        root().join("staging").join(&id).join("package")
    );
}

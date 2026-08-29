use super::*;

#[test]
fn key_ids_are_normalized_to_uppercase_hex() {
    assert_eq!(
        normalize_key_id("abcdef0123456789").unwrap(),
        "ABCDEF0123456789"
    );
    assert_eq!(
        normalize_key_id("ABCD EF01 2345 6789").unwrap(),
        "ABCDEF0123456789"
    );
    assert!(normalize_key_id("short").is_err());
    assert!(normalize_key_id("zzzzzzzzzzzzzzzz").is_err());
}

#[test]
fn a_missing_signature_file_is_reported_as_absent_not_verified() {
    let dir = crate::update::tests::scratch_root("signature-absent");
    let document = dir.join("manifest.json");
    std::fs::write(&document, b"{}\n").unwrap();
    let verdict = verify_detached(&document, &dir.join("manifest.json.asc"), &[]);
    assert_eq!(verdict, Signature::Absent);
    assert!(!verdict.is_verified());
}

#[test]
fn a_signature_with_no_keyring_is_unverifiable_never_verified() {
    let dir = crate::update::tests::scratch_root("signature-no-keyring");
    let document = dir.join("manifest.json");
    let detached = dir.join("manifest.json.asc");
    std::fs::write(&document, b"{}\n").unwrap();
    std::fs::write(&detached, b"-----BEGIN PGP SIGNATURE-----\n").unwrap();
    let verdict = verify_detached(&document, &detached, &[]);
    assert!(matches!(verdict, Signature::Unverifiable { .. }));
    assert!(!verdict.is_verified());
}

#[test]
fn only_a_status_line_with_a_good_signature_yields_a_key() {
    assert_eq!(
        good_signature_key("[GNUPG:] VALIDSIG ABCDEF0123456789ABCDEF0123456789ABCDEF01 x y\n"),
        Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01".to_string())
    );
    assert_eq!(
        good_signature_key("[GNUPG:] GOODSIG ABCDEF0123456789 Claw OS\n"),
        Some("ABCDEF0123456789".to_string())
    );
    assert_eq!(
        good_signature_key("[GNUPG:] EXPKEYSIG ABCDEF0123456789\n"),
        None
    );
    assert_eq!(good_signature_key("gpgv: Good signature\n"), None);
}

#[test]
fn keyring_discovery_prefers_operator_roots_then_the_apt_keyring() {
    let root = crate::update::tests::scratch_root("signature-keyrings");
    let operator = joined(&root, crate::update::OPERATOR_KEYRING_DIR);
    std::fs::create_dir_all(&operator).unwrap();
    std::fs::write(operator.join("10-rotation.gpg"), b"x").unwrap();
    std::fs::write(operator.join("notes.txt"), b"x").unwrap();
    let apt = joined(&root, crate::update::APT_KEYRING);
    std::fs::create_dir_all(apt.parent().unwrap()).unwrap();
    std::fs::write(&apt, b"x").unwrap();

    let found = keyrings(&root, crate::update::APT_KEYRING);
    assert_eq!(found, vec![operator.join("10-rotation.gpg"), apt]);
}

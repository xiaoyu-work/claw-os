use super::*;

#[test]
fn handles_are_unique_and_non_enumerable() {
    let first = GrantHandle::generate().expect("mint a handle");
    let second = GrantHandle::generate().expect("mint a handle");
    let first_key = first.key();
    assert_ne!(first_key, second.key());

    let wire = first.into_wire();
    // 32 bytes of entropy, hex-encoded.
    assert_eq!(wire.len(), 64);
    assert!(wire.chars().all(|c| c.is_ascii_hexdigit()));
    // Presenting the same characters resolves to the same store key.
    assert_eq!(HandleKey::of(&wire), first_key);
}

#[test]
fn handles_never_render_their_characters() {
    let handle = GrantHandle::generate().expect("mint a handle");
    let key = handle.key();
    assert_eq!(format!("{handle:?}"), "<grant-handle>");
    assert_eq!(format!("{handle}"), "<grant-handle>");
    assert_eq!(format!("{key:?}"), "<grant-key>");

    let wire = handle.into_wire();
    assert!(!format!("{key:?}").contains(&wire[..8]));
}

#[test]
fn audit_references_are_stable_and_do_not_reveal_the_id() {
    let first = GrantId(1).audit_ref();
    let again = GrantId(1).audit_ref();
    let other = GrantId(2).audit_ref();

    assert_eq!(first, again, "the same grant correlates across records");
    assert_ne!(first, other);
    assert!(first.as_str().starts_with("g-"));
    assert_eq!(first.as_str().len(), 2 + 16);

    // The reference is keyed, not an encoding of the id: it is neither
    // the id nor its hex, and a run of ids produces no repeats a reader
    // could count with.
    assert_ne!(first.as_str(), "g-1");
    assert_ne!(first.as_str(), format!("g-{:016x}", 1u64));
    let mut seen = std::collections::BTreeSet::new();
    for id in 1..=256u64 {
        assert!(
            seen.insert(GrantId(id).audit_ref()),
            "reference collision at id {id}"
        );
    }
}

#[test]
fn a_guessed_handle_has_its_own_key() {
    let guess = "0".repeat(64);
    let real = GrantHandle::generate().expect("mint a handle");
    assert_ne!(HandleKey::of(&guess), real.key());
}

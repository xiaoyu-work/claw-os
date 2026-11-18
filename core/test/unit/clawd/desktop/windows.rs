use super::*;

#[test]
fn window_references_refuse_reused_ids_from_a_different_display() {
    let epoch = Epoch([0x12; 16]);
    let reference = format!("{}:402653184", epoch.label());
    assert_eq!(bound_identifier(&reference, epoch).unwrap(), 402653184);
    assert!(bound_identifier(&reference, Epoch([0x34; 16])).is_err());
}

#[test]
fn window_references_are_closed_and_canonical() {
    let epoch = Epoch([0xab; 16]);
    let label = epoch.label();
    for reference in [
        "window-1".to_string(),
        format!("{}:1", label.to_uppercase()),
        format!("{label}:0"),
        format!("{label}:01"),
        format!("{label}:+1"),
        format!("{label}:-1"),
        format!("{label}:18446744073709551616"),
        format!("{label}:1:2"),
        format!("{label}:1\n"),
        format!("{label}/1"),
    ] {
        assert!(parse_identifier(&reference).is_err(), "{reference:?}");
    }
}

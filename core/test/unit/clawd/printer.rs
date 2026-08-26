use super::*;

#[test]
fn cups_job_ids_are_strict() {
    assert!(valid_job_id("office-123"));
    assert!(!valid_job_id("--help"));
    assert!(!valid_job_id("office-not-a-number"));
}

#[test]
fn sides_options_are_bounded() {
    validate_action(
        "print",
        Some("office"),
        Some("/tmp/document"),
        None,
        None,
        None,
        Some("two-sided-long-edge"),
        1,
        false,
    )
    .unwrap();
}

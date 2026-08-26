use super::*;

#[test]
fn short_id_takes_last_8_hex_of_random_tail() {
    // tail = "e71a8d6a8ca4" (12 hex), last 8 = "8d6a8ca4"
    assert_eq!(short_id("ses_0019e2566eb1f_e71a8d6a8ca4"), "…8d6a8ca4");
}

#[test]
fn short_id_handles_short_tails() {
    assert_eq!(short_id("ses_short_xyz"), "…xyz");
}

#[test]
fn short_id_falls_back_on_malformed_input() {
    assert_eq!(short_id("nopeunderscores"), "nopeunderscores");
}

#[test]
fn parse_rfc3339_z_handles_canonical_input() {
    // 2025-01-01T00:00:00Z = 1735689600
    assert_eq!(parse_rfc3339_z("2025-01-01T00:00:00Z"), Some(1735689600));
    // 1970-01-01T00:00:00Z = 0
    assert_eq!(parse_rfc3339_z("1970-01-01T00:00:00Z"), Some(0));
}

#[test]
fn parse_rfc3339_z_rejects_garbage() {
    assert_eq!(parse_rfc3339_z(""), None);
    assert_eq!(parse_rfc3339_z("not-a-time"), None);
    assert_eq!(parse_rfc3339_z("2025-01-01T00:00:00"), None); // no Z
}

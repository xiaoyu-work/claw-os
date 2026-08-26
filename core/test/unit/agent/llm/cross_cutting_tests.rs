use super::*;

#[test]
fn truncate_for_display_handles_unicode() {
    // The classic byte-slice trap: "héllo" is 6 bytes long;
    // s[..4] would panic since byte 4 is inside the multi-byte é.
    // truncate_for_display works on char-boundaries.
    let s = "héllo wörld";
    let t = truncate_for_display(s, 4);
    assert_eq!(t, "héll…");
}

#[test]
fn truncate_for_display_passes_short_strings_through() {
    assert_eq!(truncate_for_display("hi", 10), "hi");
    assert_eq!(truncate_for_display("", 10), "");
}

#[test]
fn redact_body_for_error_masks_long_tokens() {
    let body = r#"{"error":"bad key sk-abcdef1234567890abcdef1234567890"}"#;
    let r = redact_body_for_error(body);
    assert!(!r.contains("sk-abcdef1234567890abcdef1234567890"), "raw key leaked: {r}");
    // Some marker should remain so we can correlate.
    assert!(r.contains("***"));
}

#[test]
fn redact_body_for_error_caps_length() {
    let body = "x".repeat(10_000);
    let r = redact_body_for_error(&body);
    // 200 char cap + possible ellipsis.
    assert!(r.chars().count() <= 201, "len = {}", r.chars().count());
}

use super::*;

#[test]
fn parse_rfc3339_round_trips_now() {
    let s = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let parsed = parse_rfc3339(&s).expect("parse");
    let drift = SystemTime::now()
        .duration_since(parsed)
        .unwrap_or_default();
    assert!(drift < Duration::from_secs(2), "drift = {drift:?}");
}

#[test]
fn parse_rfc3339_rejects_garbage() {
    assert!(parse_rfc3339("not a timestamp").is_none());
    assert!(parse_rfc3339("").is_none());
}

#[test]
fn archive_path_is_under_archive_root() {
    let sid: SessionId = "ses_018f4ae0c2300_a1b2c3d4e5f6".parse().unwrap();
    let p = archive_path(&sid);
    assert!(p.ends_with(".archive/ses_018f4ae0c2300_a1b2c3d4e5f6.zip"));
}

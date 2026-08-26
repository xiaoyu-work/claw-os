use super::*;

#[test]
fn generate_yields_canonical_shape() {
    let id = SessionId::generate();
    let s = id.as_str();
    assert!(s.starts_with("ses_"), "{s}");
    assert_eq!(s.len(), 4 + 13 + 1 + 12);
    let _: SessionId = s.parse().unwrap();
}

#[test]
fn generate_is_unique_within_a_burst() {
    use std::collections::HashSet;
    let ids: HashSet<_> = (0..1024).map(|_| SessionId::generate().into_string()).collect();
    assert_eq!(ids.len(), 1024, "collision in 1024 burst");
}

#[test]
fn from_str_rejects_path_traversal() {
    assert!("../etc/passwd".parse::<SessionId>().is_err());
    assert!("ses_..".parse::<SessionId>().is_err());
    assert!("ses_/etc".parse::<SessionId>().is_err());
}

#[test]
fn from_str_rejects_short_or_long() {
    assert!("ses_".parse::<SessionId>().is_err());
    assert!("ses_0".parse::<SessionId>().is_err());
    assert!(format!("ses_{}_{}xx", "0".repeat(13), "0".repeat(12))
        .parse::<SessionId>()
        .is_err());
}

#[test]
fn from_str_rejects_non_hex() {
    let bad = format!("ses_{}_{}", "0".repeat(13), "ZZZZZZZZZZZZ");
    assert!(bad.parse::<SessionId>().is_err());
}

#[test]
fn from_str_accepts_generated_ids() {
    for _ in 0..32 {
        let id = SessionId::generate();
        let round: SessionId = id.as_str().parse().unwrap();
        assert_eq!(id, round);
    }
}

#[test]
fn serde_round_trip() {
    let id = SessionId::generate();
    let json = serde_json::to_string(&id).unwrap();
    let back: SessionId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

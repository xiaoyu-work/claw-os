use super::normalized;

#[test]
fn normalizes_integrated_and_prefixed_queries() {
    assert_eq!(normalized(""), Some(String::new()));
    assert_eq!(normalized("report"), Some("report".into()));
    assert_eq!(normalized("recent Report"), Some("report".into()));
}

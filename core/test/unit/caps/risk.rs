use super::*;

#[test]
fn ordering_lets_us_take_the_max() {
    let risks = [Risk::Low, Risk::Critical, Risk::Medium, Risk::High];
    assert_eq!(risks.iter().copied().max(), Some(Risk::Critical));
}

#[test]
fn ord_is_total_and_intuitive() {
    assert!(Risk::Low < Risk::Medium);
    assert!(Risk::Medium < Risk::High);
    assert!(Risk::High < Risk::Critical);
}

#[test]
fn label_returns_english_string() {
    assert_eq!(Risk::Critical.label().en(), "Critical risk");
    assert_eq!(Risk::Critical.as_str(), "critical");
}

#[test]
fn serde_uses_lowercase_strings() {
    let v = serde_json::to_string(&Risk::High).unwrap();
    assert_eq!(v, "\"high\"");
    let back: Risk = serde_json::from_str("\"critical\"").unwrap();
    assert_eq!(back, Risk::Critical);
}

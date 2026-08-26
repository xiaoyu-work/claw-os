use super::*;

#[test]
fn parse_bare_string_means_english() {
    let t: LocalizedText = serde_json::from_str(r#""Files""#).unwrap();
    assert_eq!(t.en_str(), "Files");
    assert!(t.has_english());
}

#[test]
fn parse_object_form() {
    let t: LocalizedText =
        serde_json::from_str(r#"{"en": "Files", "zh-CN": "文件"}"#).unwrap();
    assert_eq!(t.en_str(), "Files");
    assert_eq!(t.get(Locale::En), "Files");
    // The fallback works for an unrecognised locale even though our
    // Locale enum only has En right now.
}

#[test]
fn validate_rejects_missing_english() {
    let t = LocalizedText::default();
    assert!(t.validate().is_err());

    let t: LocalizedText = serde_json::from_str(r#"{"zh-CN": "文件"}"#).unwrap();
    assert!(t.validate().is_err());
}

#[test]
fn empty_english_counts_as_missing() {
    let t = LocalizedText::en("   ");
    assert!(!t.has_english());
    assert!(t.validate().is_err());
}

#[test]
fn serde_round_trip_preserves_object_form() {
    let original = LocalizedText::en("hi").with("zh-CN", "你");
    let j = serde_json::to_string(&original).unwrap();
    let back: LocalizedText = serde_json::from_str(&j).unwrap();
    assert_eq!(back, original);
}

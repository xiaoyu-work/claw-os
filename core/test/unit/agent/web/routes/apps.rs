use super::*;

#[test]
fn app_reviews_preserve_full_width_display_revisions() {
    let value = project_review(json!({"review": {"revision": u64::MAX}})).unwrap();
    assert_eq!(value["review"]["revision"], u64::MAX.to_string());
    assert!(project_review(json!({"review": {"revision": "not-the-root-wire-type"}})).is_err());
}

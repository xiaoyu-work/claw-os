use super::*;

#[test]
fn package_impact_never_uses_app_installation_confirmation() {
    assert!(app_confirmation_action(ReviewKind::PackageImpact).is_err());
    assert!(project_review(json!({"review":{
        "kind":"package_impact","revision":7
    }})).is_err());
    assert!(project_review(json!({"reviews":[
        {"kind":"install","revision":1},
        {"kind":"package_impact","revision":7}
    ]})).is_err());
    assert_eq!(app_confirmation_action(ReviewKind::Install).unwrap(), ReviewAction::ConfirmInstall);
}

#[test]
fn app_reviews_preserve_full_width_display_revisions() {
    let value = project_review(json!({
        "review": {"revision": u64::MAX},
        "reviews": [{"revision": 7}, {"revision": u64::MAX}]
    })).unwrap();
    assert_eq!(value["review"]["revision"], u64::MAX.to_string());
    assert_eq!(value["reviews"][0]["revision"], "7");
    assert_eq!(value["reviews"][1]["revision"], u64::MAX.to_string());
    assert!(project_review(json!({"review": {"revision": "not-the-root-wire-type"}})).is_err());
    assert!(project_review(json!({"reviews": [{"revision": "not-the-root-wire-type"}]})).is_err());
    assert!(project_review(json!({"reviews": {}})).is_err());
}

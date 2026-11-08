use super::*;

#[test]
fn app_views_are_owner_and_browser_bound_and_expire() {
    let _guard = crate::test_env::lock_env();
    let package = crate::test_env::signed_web_app("isolated-view");
    let caller = AuthenticatedToken {
        uid: 1000,
        token_id: "browser-one".into(),
        expires_at: u64::MAX,
    };
    let views = Views::default();
    let id = views
        .create(
            &caller,
            package.clone(),
            PackageRef::of(&package),
            crate::apps::store::web_metadata(&package).unwrap(),
        )
        .unwrap();
    assert!(views.owned(&id, &caller).is_ok());
    let foreign = AuthenticatedToken {
        uid: 1001,
        ..caller.clone()
    };
    assert!(views.owned(&id, &foreign).is_err());
    let browser = AuthenticatedToken {
        token_id: "browser-two".into(),
        ..caller.clone()
    };
    assert!(views.owned(&id, &browser).is_err());
    views.current.lock().unwrap().get_mut(&id).unwrap().expires = Instant::now();
    assert!(views.resource(&id).is_err());
}

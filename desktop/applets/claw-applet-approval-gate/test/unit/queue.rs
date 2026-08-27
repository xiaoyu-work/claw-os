use super::{GrantDuration, Scope};

#[test]
fn queue_business_types_remain_independent_of_transport() {
    assert_eq!(
        Scope::Path("/home/user".to_string()).render(),
        "path:/home/user"
    );
    assert_eq!(
        serde_json::to_value(GrantDuration::Session).unwrap(),
        serde_json::json!("session")
    );
}

use super::*;
use serde_json::json;

#[test]
fn anchors_reverse_direction_without_skipping_the_anchor() {
    let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let params = Params::new(
        json!({ "cursor": encode("thread", "b", false) }),
        &["cursor"],
    )
    .unwrap();
    assert_eq!(start(&params, "thread", &ids).unwrap(), 2);
    let params = Params::new(
        json!({ "cursor": encode("thread", "b", true) }),
        &["cursor"],
    )
    .unwrap();
    let reversed = ids.into_iter().rev().collect::<Vec<_>>();
    assert_eq!(start(&params, "thread", &reversed).unwrap(), 1);
}

#[test]
fn cursor_is_bound_to_its_query_and_existing_identity() {
    let params = Params::new(
        json!({ "cursor": encode("other-thread", "a", false) }),
        &["cursor"],
    )
    .unwrap();
    assert!(start(&params, "thread", &["a".into()]).is_err());
    let params = Params::new(
        json!({ "cursor": encode("thread", "deleted", false) }),
        &["cursor"],
    )
    .unwrap();
    assert_eq!(
        start(&params, "thread", &["a".into()]).unwrap_err().code,
        -32003
    );
    assert_eq!(
        scope(
            "threads",
            &json!({ "archived": false, "limit": 1, "sortDirection": "asc" })
        ),
        scope(
            "threads",
            &json!({ "archived": false, "limit": 50, "sortDirection": "desc" })
        ),
    );
}

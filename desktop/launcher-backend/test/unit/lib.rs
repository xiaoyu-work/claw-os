use super::*;

#[test]
fn search_result_category_is_backward_compatible() {
    let result: SearchResult =
        serde_json::from_str(r#"{"id":1,"name":"Example","description":""}"#).unwrap();

    assert_eq!(result.category, SearchResultCategory::Unknown);
}

#[test]
fn search_result_category_round_trips() {
    let result = SearchResult {
        id: 3,
        name: "Settings".into(),
        description: "System settings".into(),
        icon: None,
        category_icon: None,
        window: None,
        category: SearchResultCategory::Settings,
    };

    let json = serde_json::to_string(&result).unwrap();
    let decoded: SearchResult = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.category, SearchResultCategory::Settings);
}

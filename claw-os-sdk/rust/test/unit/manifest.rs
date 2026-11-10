use super::*;

#[test]
fn owner_path_shared_vectors_are_bounded_lexical_previews() {
    let vectors: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"), "/../wire/v1/owner_path.vectors.json",
    ))).unwrap();
    for case in vectors["cases"].as_array().unwrap() {
        let result = resolve_owner_path(case["verb"].as_str().unwrap(), case["path"].as_str().unwrap(), case["home"].as_str().unwrap());
        match case["expected"].as_str() {
            Some(expected) => assert_eq!(result.unwrap(), expected, "{}", case["name"]),
            None => assert!(result.is_err(), "{}", case["name"]),
        }
    }
    assert!(validate_owner_path("fs.read", &"é".repeat(128)).is_err());
    for field in ["owner_uid", "home", "owner_home"] {
        let mut need = json!({"verb":"fs.read","scope":{"kind":"owner-path","path":"Documents/**"}});
        need["scope"][field] = json!("/home/bob");
        assert!(super::super::generated::validate_owner_path_need(&need).is_err());
    }
}

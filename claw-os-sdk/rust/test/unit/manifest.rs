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

        #[test]
        fn optional_gui_lists_reject_null_and_invalid_owner_paths_on_both_surfaces() {
            for invalid in [Value::Null, json!("not-an-array"), json!({}), json!([{
                "verb":"fs.read","scope":{"kind":"owner-path","path":"../other/**"},
            }])] {
                for value in [
                    json!({"desktop":{"optional_needs":invalid.clone()}}),
                    json!({"desktop":{"entries":{"applet":{"optional_needs":invalid.clone()}}}}),
                ] {
                    assert!(validate_owner_path_bindings(&value).is_err());
                }
            }
            validate_owner_path_bindings(&json!({"desktop":{"optional_needs":[]}})).unwrap();
            validate_owner_path_bindings(&json!({"desktop":{"entries":{"applet":{"optional_needs":[{
                "verb":"fs.read","scope":{"kind":"owner-path","path":"Documents/**"},
            }]}}}})).unwrap();
        }
    }
    assert!(validate_owner_path("fs.read", &"é".repeat(128)).is_err());
    for field in ["owner_uid", "home", "owner_home"] {
        let mut need = json!({"verb":"fs.read","scope":{"kind":"owner-path","path":"Documents/**"}});
        need["scope"][field] = json!("/home/bob");
        assert!(super::super::generated::validate_owner_path_need(&need).is_err());
    }
}

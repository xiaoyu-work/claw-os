use super::*;
use serde_json::json;

#[test]
fn a_token_is_trimmed_bounded_and_charset_checked() {
    assert_eq!(Token::<8>::parse("  ok-1  ").unwrap().as_str(), "ok-1");
    assert!(Token::<8>::parse("012345678").is_err());
    assert!(Token::<64>::parse("has space").is_err());
    assert!(Token::<64>::parse("semi;colon").is_err());
    // Empty stays representable: routes already treat an empty optional
    // string as absent, and rejecting it here would change behaviour
    // rather than close a hole.
    assert_eq!(Token::<8>::parse("").unwrap().as_str(), "");
}

#[test]
fn a_name_accepts_paths_and_units_but_not_arbitrary_text() {
    assert_eq!(
        Name::<64>::parse("/dev/sda1").unwrap().as_str(),
        "/dev/sda1"
    );
    assert_eq!(
        Name::<64>::parse("ssh.service").unwrap().as_str(),
        "ssh.service"
    );
    assert_eq!(
        Name::<64>::parse("text/plain").unwrap().as_str(),
        "text/plain"
    );
    assert!(Name::<64>::parse("rm -rf /").is_err());
    assert!(Name::<4>::parse("toolong").is_err());
}

#[test]
fn free_text_keeps_newlines_but_refuses_control_bytes_and_length() {
    assert!(Text::<32>::parse("line one\nline two\t.").is_ok());
    assert!(Text::<32>::parse("nul\0byte").is_err());
    assert!(Text::<4>::parse("12345").is_err());
}

#[test]
fn a_wait_is_capped_at_the_broker_ceiling() {
    assert_eq!(
        serde_json::from_value::<WaitMillis>(json!(1000))
            .unwrap()
            .get(),
        1000
    );
    assert!(serde_json::from_value::<WaitMillis>(json!(MAX_WAIT_MS)).is_ok());
    assert!(serde_json::from_value::<WaitMillis>(json!(MAX_WAIT_MS + 1)).is_err());
    assert!(serde_json::from_value::<WaitMillis>(json!(u64::MAX)).is_err());
}

#[test]
fn a_text_list_is_bounded_in_both_dimensions() {
    let ok = json!(["a", "b"]);
    assert_eq!(
        serde_json::from_value::<TextList<4, 8>>(ok)
            .unwrap()
            .as_slice()
            .len(),
        2
    );
    let too_many = json!(["a", "b", "c", "d", "e"]);
    assert!(serde_json::from_value::<TextList<4, 8>>(too_many).is_err());
    let item_too_long = json!(["123456789"]);
    assert!(serde_json::from_value::<TextList<4, 8>>(item_too_long).is_err());
    let not_strings = json!([1, 2]);
    assert!(serde_json::from_value::<TextList<4, 8>>(not_strings).is_err());
}

fn nest(depth: usize) -> Value {
    let mut value = json!(1);
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

#[test]
fn structured_payloads_are_bounded_in_depth_width_and_size() {
    assert!(Structured::parse(nest(MAX_STRUCTURED_DEPTH)).is_ok());
    assert!(Structured::parse(nest(MAX_STRUCTURED_DEPTH + 2)).is_err());

    let wide = Value::Array(vec![json!(1); MAX_STRUCTURED_ARRAY_LEN + 1]);
    assert!(Structured::parse(wide).is_err());

    let mut object = serde_json::Map::new();
    for index in 0..=MAX_STRUCTURED_OBJECT_LEN {
        object.insert(format!("k{index}"), json!(1));
    }

    assert!(Structured::parse(Value::Object(object)).is_err());

    let long_string = Value::String("x".repeat(MAX_STRUCTURED_STRING_BYTES + 1));
    assert!(Structured::parse(long_string).is_err());

    let long_key = {
        let mut map = serde_json::Map::new();
        map.insert("k".repeat(MAX_STRUCTURED_KEY_BYTES + 1), json!(1));
        Value::Object(map)
    };
    assert!(Structured::parse(long_key).is_err());
}

#[test]
fn cli_mcp_arguments_allow_bounded_content_without_relaxing_metadata_limits() {
    let content = json!({"content": "text\0".repeat(32 * 1024)});
    assert!(Structured::parse(content.clone()).is_err());
    assert!(McpArguments::parse(content).is_ok());
    assert!(McpArguments::parse(json!({"content":"x".repeat(APP_ARGS_STDIN_MAX_BYTES)})).is_err());
    assert!(McpArguments::parse(json!({"content":nest(MAX_STRUCTURED_DEPTH + 1)})).is_err());
    assert!(McpArguments::parse(json!({"content":vec![0; MAX_STRUCTURED_ARRAY_LEN + 1]})).is_err());
    assert!(McpArguments::parse(json!([])).is_err());
}

#[test]
fn a_wide_but_shallow_payload_is_refused_on_node_count() {
    // 4 x 1024 array entries is inside every individual bound but past
    // the total node budget, so a "many small arrays" shape cannot be
    // used to smuggle an expensive document past the depth check.
    let row = Value::Array(vec![json!(1); MAX_STRUCTURED_ARRAY_LEN]);
    let grid = Value::Array(vec![row; 8]);
    assert!(Structured::parse(grid).is_err());
}

#[test]
fn structured_payloads_survive_the_canonical_round_trip() {
    let original = json!({
        "kind": "path",
        "path": "/home/user/docs",
        "limits": [1, 2.5, -3],
        "nested": {"flag": true, "null": null},
    });
    let structured = Structured::parse(original.clone()).unwrap();
    let encoded = serde_json::to_value(&structured).unwrap();
    assert_eq!(encoded, original);
}

#[test]
fn a_route_with_no_body_refuses_arguments() {
    assert!(serde_json::from_value::<NoParams>(json!({})).is_ok());
    assert!(serde_json::from_value::<NoParams>(json!({"limit": 1})).is_err());
    assert!(serde_json::from_value::<NoParams>(json!("text")).is_err());
}

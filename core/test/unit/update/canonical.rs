use super::*;

use serde_json::json;

#[test]
fn object_keys_are_sorted_and_compact() {
    let value = json!({"b": 1, "a": {"z": true, "y": [1, 2]}});
    assert_eq!(
        to_string(&value).unwrap(),
        r#"{"a":{"y":[1,2],"z":true},"b":1}"#
    );
}

#[test]
fn floating_point_numbers_are_refused() {
    let value = json!({"weight": 1.5});
    assert!(to_string(&value).is_err());
}

#[test]
fn control_characters_are_escaped() {
    let value = json!({"text": "a\nb\tc\u{1}"});
    assert_eq!(to_string(&value).unwrap(), r#"{"text":"a\nb\tc\u0001"}"#);
}

#[test]
fn a_non_canonical_encoding_is_refused_rather_than_normalized() {
    let pretty = b"{\n  \"a\": 1\n}";
    assert!(parse_canonical(pretty).is_err());
    let reordered = br#"{"b":1,"a":2}"#;
    assert!(parse_canonical(reordered).is_err());
}

#[test]
fn canonical_bytes_round_trip_with_and_without_the_trailing_newline() {
    let value = json!({"a": 1, "b": "two"});
    let bytes = to_bytes(&value).unwrap();
    assert!(bytes.ends_with(b"\n"));
    assert_eq!(parse_canonical(&bytes).unwrap(), value);
    assert_eq!(parse_canonical(&bytes[..bytes.len() - 1]).unwrap(), value);
}

#[test]
fn embedded_newlines_are_refused_so_a_history_line_cannot_be_forged() {
    let injected = b"{\"a\":1}\n{\"a\":2}";
    assert!(parse_canonical(injected).is_err());
}

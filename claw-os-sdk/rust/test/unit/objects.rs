use super::*;
use crate::generated::{validate_object_ref, Manifest};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Vectors {
    valid: Vec<Canonical>,
    invalid_uris: Vec<String>,
    invalid_refs: Vec<ObjectRef>,
    limits: Vec<Limit>,
    wire_cases: Vec<WireCase>,
}

#[derive(Deserialize)]
struct Canonical {
    reference: ObjectRef,
    uri: String,
}

#[derive(Deserialize)]
struct Limit {
    field: String,
    unit: String,
    count: usize,
    bytes: usize,
}

#[derive(Deserialize)]
struct WireCase {
    value: Value,
    code: Option<String>,
    path: Option<String>,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wire/v1/object_ref.vectors.json"
    )))
    .unwrap()
}

fn reference() -> ObjectRef {
    ObjectRef {
        app_id: "notes".to_string(),
        object_type: "note".to_string(),
        object_id: "x".to_string(),
        revision: None,
    }
}

#[test]
fn shared_canonical_vectors_round_trip_without_identity_changes() {
    for case in vectors().valid {
        validate(&case.reference).unwrap();
        assert_eq!(format_reference(&case.reference).unwrap(), case.uri);
        let parsed = parse_reference(&case.uri).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::to_value(case.reference).unwrap(),
            "{}",
            case.uri
        );
    }
}

#[test]
fn shared_invalid_vectors_fail_without_permissive_url_normalization() {
    for uri in vectors().invalid_uris {
        assert!(parse_reference(&uri).is_err(), "{uri}");
    }
    for reference in vectors().invalid_refs {
        assert!(validate(&reference).is_err(), "{reference:?}");
        assert!(format_reference(&reference).is_err(), "{reference:?}");
    }
}

#[test]
fn shared_byte_limits_accept_the_boundary_and_reject_one_more_byte() {
    for limit in vectors().limits {
        let text = limit.unit.repeat(limit.count);
        assert_eq!(text.len(), limit.bytes);
        let mut value = serde_json::to_value(reference()).unwrap();
        value[&limit.field] = json!(text);
        let valid: ObjectRef = serde_json::from_value(value.clone()).unwrap();
        let uri = format_reference(&valid).unwrap();
        assert!(parse_reference(&uri).is_ok());
        value[&limit.field] = json!(format!("{text}x"));
        let invalid: ObjectRef = serde_json::from_value(value).unwrap();
        assert!(validate(&invalid).is_err(), "{}", limit.field);
        assert!(format_reference(&invalid).is_err(), "{}", limit.field);
    }
}

#[test]
fn maximum_components_fit_the_uri_ceiling_and_oversized_uris_fail() {
    let value = ObjectRef {
        app_id: "a".repeat(128),
        object_type: "a".repeat(64),
        object_id: "\u{e9}".repeat(512),
        revision: Some("\u{e9}".repeat(64)),
    };
    let uri = format_reference(&value).unwrap();
    assert_eq!(uri.len(), 3669);
    assert!(parse_reference(&uri).is_ok());
    assert!(parse_reference(&format!("app://notes/note?id={}", "x".repeat(4096))).is_err());
}

#[test]
fn generated_object_ref_decoder_matches_the_shared_closed_contract() {
    for case in vectors().wire_cases {
        match case.code {
            Some(code) => {
                let error = validate_object_ref(&case.value).unwrap_err();
                assert_eq!(error.code, code);
                assert_eq!(Some(error.path.as_str()), case.path.as_deref());
            }
            None => {
                validate_object_ref(&case.value).unwrap();
                let typed: ObjectRef = serde_json::from_value(case.value.clone()).unwrap();
                assert_eq!(serde_json::to_value(typed).unwrap(), case.value);
            }
        }
    }
}

#[test]
fn legacy_manifests_and_unrevisioned_references_remain_compatible() {
    let value = json!({"id": "notes", "version": "1.0.0", "name": {"en": "Notes"}});
    let manifest: Manifest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(manifest).unwrap(), value);
    let minimal = serde_json::to_value(reference()).unwrap();
    assert!(minimal.get("revision").is_none());
    assert!(minimal.get("wire_version").is_none());
}

#[test]
fn object_ref_error_is_public_and_implements_standard_error() {
    let error: crate::ObjectRefError = parse_reference("").unwrap_err();
    let error: &dyn std::error::Error = &error;
    assert!(error
        .to_string()
        .starts_with("invalid App object reference:"));
}

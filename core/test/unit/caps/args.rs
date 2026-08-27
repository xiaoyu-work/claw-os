use super::*;

use crate::caps::manifest::{Arg, ArgKind};

fn decls() -> Vec<Arg> {
    serde_json::from_value(serde_json::json!([
        {"name": "path", "kind": "path", "required": true},
        {"name": "recursive", "kind": "bool", "binding": "flag"},
        {"name": "limit", "kind": "integer", "binding": "flag", "default": 10}
    ]))
    .expect("arg declarations")
}

fn raw(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| (*token).to_string()).collect()
}

#[test]
fn declared_binding_controls_how_values_are_bound() {
    let decls = decls();
    let values = bind_cli_args(
        &decls,
        &raw(&["/home/u/notes.txt", "--recursive", "--limit=7"]),
    )
    .unwrap();
    assert_eq!(
        values.get("path"),
        Some(&Value::String("/home/u/notes.txt".to_string()))
    );
    assert_eq!(values.get("recursive"), Some(&Value::Bool(true)));
    assert_eq!(values.get("limit").and_then(Value::as_i64), Some(7));
}

#[test]
fn unknown_flags_are_rejected() {
    let error = bind_cli_args(&decls(), &raw(&["--bogus", "/etc/shadow"]))
        .expect_err("undeclared flags must fail closed");
    assert!(error.contains("--bogus"), "unexpected error: {error}");
}

#[test]
fn legacy_boolean_binding_is_a_flag() {
    let decls: Vec<Arg> = serde_json::from_value(serde_json::json!([
        {"name": "urgent", "kind": "bool"}
    ]))
    .unwrap();
    let values = bind_cli_args(&decls, &raw(&["--urgent"])).unwrap();
    assert_eq!(values["urgent"], Value::Bool(true));
}

#[test]
fn invalid_supplied_scalars_are_rejected() {
    for declaration in [
        serde_json::json!({"name": "value", "kind": "number"}),
        serde_json::json!({"name": "value", "kind": "integer"}),
        serde_json::json!({"name": "value", "kind": "bool", "binding": "positional"}),
    ] {
        let decls: Vec<Arg> = serde_json::from_value(Value::Array(vec![declaration])).unwrap();
        let error = bind_cli_args(&decls, &raw(&["not-a-value"]))
            .expect_err("invalid supplied values must not reach an App");
        assert!(error.contains("value"), "unexpected error: {error}");
    }

    let bool_flag: Vec<Arg> = serde_json::from_value(serde_json::json!([
        {"name": "urgent", "kind": "bool"}
    ]))
    .unwrap();
    assert!(bind_cli_args(&bool_flag, &raw(&["--urgent=maybe"])).is_err());
}

#[test]
fn end_of_options_allows_flag_shaped_positionals() {
    let decls: Vec<Arg> = serde_json::from_value(serde_json::json!([
        {"name": "text", "kind": "text", "required": true}
    ]))
    .unwrap();
    let values = bind_cli_args(&decls, &raw(&["--", "--literal"])).unwrap();
    assert_eq!(values["text"], Value::String("--literal".to_string()));
}

#[test]
fn kebab_alias_and_defaults_are_applied() {
    let decls: Vec<Arg> = serde_json::from_value(serde_json::json!([
        {"name": "dry_run", "kind": "bool", "binding": "flag"},
        {"name": "limit", "kind": "integer", "binding": "flag", "default": 7}
    ]))
    .expect("arg declarations");
    let values = bind_cli_args(&decls, &raw(&["--dry-run"])).unwrap();
    assert_eq!(values.get("dry_run"), Some(&Value::Bool(true)));
    assert_eq!(
        values.get("limit").and_then(Value::as_f64),
        Some(7.0),
        "declared default should be bound when the flag is absent"
    );
}

#[test]
fn missing_required_argument_is_rejected() {
    let decls = decls();
    let values = bind_cli_args(&decls, &raw(&["--recursive"])).unwrap();
    let error = validate_bound_args(&decls, &values).expect_err("path is required");
    assert!(error.contains("path"), "unexpected error: {error}");
}

#[test]
fn mistyped_declared_arguments_are_rejected_and_extras_ignored() {
    let decls = decls();
    let mut values = BTreeMap::new();
    values.insert("path".to_string(), Value::String("/home/u".to_string()));
    values.insert("smuggled".to_string(), Value::String("x".to_string()));
    assert!(
        validate_bound_args(&decls, &values).is_ok(),
        "an undeclared key cannot bind a scope, so it is not an authorization concern"
    );

    let mut values = BTreeMap::new();
    values.insert("path".to_string(), Value::Bool(true));
    let error = validate_bound_args(&decls, &values).expect_err("path must be a string");
    assert!(error.contains("path"), "unexpected error: {error}");
}

#[test]
fn every_arg_kind_has_a_label() {
    for kind in [
        ArgKind::Path,
        ArgKind::Host,
        ArgKind::Name,
        ArgKind::Text,
        ArgKind::Number,
        ArgKind::Integer,
        ArgKind::Bool,
    ] {
        assert!(!kind_label(kind).is_empty());
    }
}

use super::*;

use crate::caps::manifest::{Arg, ArgKind};

fn decls() -> Vec<Arg> {
    serde_json::from_value(serde_json::json!([
        {"name": "path", "kind": "path", "required": true},
        {"name": "recursive", "kind": "bool"},
        {"name": "limit", "kind": "number", "default": 10}
    ]))
    .expect("arg declarations")
}

fn raw(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| (*token).to_string()).collect()
}

#[test]
fn positional_and_flag_forms_bind_the_same_value() {
    let decls = decls();
    let positional = bind_cli_args(&decls, &raw(&["/home/u/notes.txt"]));
    let flag = bind_cli_args(&decls, &raw(&["--path", "/home/u/notes.txt"]));
    let inline = bind_cli_args(&decls, &raw(&["--path=/home/u/notes.txt"]));
    assert_eq!(positional.get("path"), flag.get("path"));
    assert_eq!(positional.get("path"), inline.get("path"));
    assert_eq!(
        positional.get("path"),
        Some(&Value::String("/home/u/notes.txt".to_string()))
    );
}

#[test]
fn unknown_flag_value_does_not_become_a_positional_binding() {
    let values = bind_cli_args(&decls(), &raw(&["--bogus", "/etc/shadow", "/home/u/ok.txt"]));
    assert_eq!(
        values.get("path"),
        Some(&Value::String("/home/u/ok.txt".to_string()))
    );
}

#[test]
fn kebab_alias_and_defaults_are_applied() {
    let decls: Vec<Arg> = serde_json::from_value(serde_json::json!([
        {"name": "dry_run", "kind": "bool"},
        {"name": "limit", "kind": "number", "default": 7}
    ]))
    .expect("arg declarations");
    let values = bind_cli_args(&decls, &raw(&["--dry-run"]));
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
    let values = bind_cli_args(&decls, &raw(&["--recursive"]));
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
        ArgKind::Bool,
    ] {
        assert!(!kind_label(kind).is_empty());
    }
}

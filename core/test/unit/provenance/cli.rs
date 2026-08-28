use super::*;

#[test]
fn flag_parsing_accepts_both_spellings() {
    let args = vec![
        "--kind".to_string(),
        "app".to_string(),
        "--id=notes".to_string(),
        "--entrypoint".to_string(),
        "main.py".to_string(),
        "--entrypoint=lib/run.py".to_string(),
    ];
    assert_eq!(flag(&args, "kind").as_deref(), Some("app"));
    assert_eq!(flag(&args, "id").as_deref(), Some("notes"));
    assert_eq!(flag(&args, "missing"), None);
    assert_eq!(flags(&args, "entrypoint"), vec!["main.py", "lib/run.py"]);
    assert_eq!(parse_kind(&args).unwrap(), PackageKind::App);
}

#[test]
fn unknown_kind_is_refused() {
    let args = vec!["--kind".to_string(), "kernel".to_string()];
    let err = parse_kind(&args).unwrap_err();
    assert!(err.contains("unknown package kind"), "{err}");
}

#[test]
fn unknown_subcommand_lists_the_real_ones() {
    let err = run("frobnicate", &[]).unwrap_err();
    assert!(err.contains("dev-trust"), "{err}");
    assert!(err.contains("rollback"), "{err}");
}

#[test]
fn trust_roots_are_reported_as_compiled_in() {
    let value = trust_cmd(&["roots".to_string()]).unwrap();
    let note = value["note"].as_str().unwrap();
    assert!(note.contains("No environment variable"), "{note}");
    assert!(!value["roots"].as_array().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn trust_add_refuses_private_key_material() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!(
        "cos-prov-cli-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("leak.json");
    let mut f = std::fs::File::create(&path).unwrap();
    let key = crate::provenance::sign::SigningKeyFile::generate(None).unwrap();
    write!(
        f,
        "{}",
        serde_json::json!({
            "schema": crate::provenance::trust::TRUST_SCHEMA_V1,
            "keys": [],
            "private_key": key.private_key,
        })
    )
    .unwrap();
    drop(f);
    let err = trust_add(&["--file".to_string(), path.display().to_string()]).unwrap_err();
    assert!(err.contains("private key material"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trust_add_requires_the_expected_schema() {
    let dir = std::env::temp_dir().join(format!(
        "cos-prov-cli-schema-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wrong.json");
    std::fs::write(&path, r#"{"schema":"claw.trust/v2","keys":[]}"#).unwrap();
    let err = trust_add(&["--file".to_string(), path.display().to_string()]).unwrap_err();
    assert!(err.contains("claw.trust/v1"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

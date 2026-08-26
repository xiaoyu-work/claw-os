use super::*;

#[test]
fn fs_write_round_trip_new_file() {
    let m = MutationRecord::new(Mutation::FsWrite {
        path: "/workspace/notes.md".into(),
        prev_blob: None,
    });
    let json = serde_json::to_string(&m).unwrap();
    let back: MutationRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn fs_write_round_trip_overwrite() {
    let m = MutationRecord::new(Mutation::FsWrite {
        path: "/workspace/notes.md".into(),
        prev_blob: Some("blob-abc123".into()),
    });
    let json = serde_json::to_string(&m).unwrap();
    let back: MutationRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn credential_round_trip() {
    let m = MutationRecord::new(Mutation::CredentialStore {
        namespace: "openai".into(),
        name: "key".into(),
        prev_value: None,
    });
    let json = serde_json::to_string(&m).unwrap();
    let back: MutationRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn opaque_escape_hatch_round_trip() {
    let m = MutationRecord::new(Mutation::Opaque {
        verb: "db.write".into(),
        forward: serde_json::json!({ "table": "x", "row": 1 }),
        inverse: serde_json::json!({ "table": "x", "delete_row": 1 }),
    });
    let json = serde_json::to_string(&m).unwrap();
    let back: MutationRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn system_service_round_trip() {
    let mutation = Mutation::SystemService {
        unit: "demo.service".into(),
        was_active: true,
        was_enabled: Some(false),
    };
    let json = serde_json::to_string(&mutation).unwrap();
    let back: Mutation = serde_json::from_str(&json).unwrap();
    assert_eq!(mutation, back);
}

#[test]
fn system_package_round_trip() {
    let mutation = Mutation::SystemPackage {
        package: "curl".into(),
        previous_version: Some("8.0-1".into()),
        was_held: true,
    };
    let json = serde_json::to_string(&mutation).unwrap();
    let back: Mutation = serde_json::from_str(&json).unwrap();
    assert_eq!(mutation, back);
}

#[test]
fn with_turn_attaches_seq() {
    let m = MutationRecord::new(Mutation::FsRename {
        from: "/a".into(),
        to: "/b".into(),
    })
    .with_turn(42);
    assert_eq!(m.turn_seq, Some(42));
}

#[test]
fn mutation_tag_is_kebab_case() {
    let json = serde_json::to_string(&Mutation::FsWrite {
        path: "/x".into(),
        prev_blob: None,
    })
    .unwrap();
    assert!(json.contains("\"kind\":\"fs-write\""), "{json}");

    let json = serde_json::to_string(&Mutation::CredentialStore {
        namespace: "ns".into(),
        name: "n".into(),
        prev_value: None,
    })
    .unwrap();
    assert!(json.contains("\"kind\":\"credential-store\""), "{json}");
}

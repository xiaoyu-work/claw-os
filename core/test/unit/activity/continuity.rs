use super::*;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn document() -> ActivityContinuityDocument {
    ActivityContinuityDocument::build(
        crate::activities::ActivityContinuityLineage {
            id: "00000000-0000-4000-8000-000000000123".into(),
            revision: 1,
        },
        crate::activities::PortableActivityIntent {
            title: "Release".into(),
            goal: "Publish".into(),
            completion_criteria: String::new(),
            boundaries: String::new(),
        },
        vec![],
        crate::activities::PortableActivityRules {
            execution_limits: None,
            scheduling: None,
        },
    )
    .unwrap()
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(format!(
            ".activity-continuity-cli-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn export_parsing_requires_an_id_and_explicit_overwrite() {
    let id = "00000000-0000-4000-8000-000000000001";
    assert_eq!(
        parse_export(&args(&[id])).unwrap(),
        ExportOptions {
            activity_id: id.into(),
            output: None,
            overwrite: false,
        }
    );
    assert_eq!(
        parse_export(&args(&[id, "--output", "activity.json", "--overwrite"])).unwrap(),
        ExportOptions {
            activity_id: id.into(),
            output: Some(PathBuf::from("activity.json")),
            overwrite: true,
        }
    );
    for invalid in [
        args(&[]),
        args(&["not-an-id"]),
        args(&[id, "--overwrite"]),
        args(&[id, "--output", "-"]),
        args(&[id, "--output", "a", "--output", "b"]),
    ] {
        assert!(parse_export(&invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn import_requires_exactly_one_bounded_source_and_explicit_local_placement() {
    assert_eq!(
        parse_import(&args(&["--placement", "local", "--file", "activity.json"])).unwrap(),
        ImportOptions {
            placement: ActivityExecutionPlacement::Local,
            input: ImportInput::File(PathBuf::from("activity.json")),
        }
    );
    assert_eq!(
        parse_import(&args(&["--stdin", "--placement", "local"])).unwrap(),
        ImportOptions {
            placement: ActivityExecutionPlacement::Local,
            input: ImportInput::Stdin,
        }
    );
    for invalid in [
        args(&["--file", "activity.json"]),
        args(&["--placement", "remote", "--file", "activity.json"]),
        args(&["--placement", "provider:a", "--stdin"]),
        args(&["--placement", "local", "--file", "-"]),
        args(&["--placement", "local", "--file", "a", "--stdin"]),
        args(&["--placement", "local"]),
    ] {
        assert!(parse_import(&invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn file_output_never_overwrites_by_default_and_round_trips() {
    let directory = Directory::new();
    let path = directory.0.join("activity.json");
    let first = document().to_json().unwrap();
    write_document(&path, &first, false).unwrap();
    assert_eq!(read_file(&path).unwrap(), first);
    assert!(write_document(&path, b"replacement", false).is_err());
    assert_eq!(read_file(&path).unwrap(), first);
    write_document(&path, b"replacement", true).unwrap();
    assert_eq!(read_file(&path).unwrap(), b"replacement");
}

#[test]
fn file_and_stream_inputs_are_regular_and_bounded() {
    let directory = Directory::new();
    assert!(read_file(&directory.0).is_err());
    assert!(read_bounded(&b"x"[..]).is_ok());
    assert!(read_bounded(&vec![b'x'; MAX_CONTINUITY_DOCUMENT_BYTES + 1][..]).is_err());
}

#[test]
fn broker_export_values_are_revalidated_before_file_output() {
    let valid = serde_json::to_value(document()).unwrap();
    assert_eq!(decode_document_value(valid).unwrap(), document());
    let mut forged = serde_json::to_value(document()).unwrap();
    forged["owner_uid"] = json!(0);
    assert!(decode_document_value(forged).is_err());
}

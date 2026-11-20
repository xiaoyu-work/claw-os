use super::*;

#[test]
fn picker_metadata_waits_for_retirement_and_cannot_carry_filesystem_authority() {
    let id = "a".repeat(32);
    let uri = "file:///nonexistent-human-selection/track.webm";
    let mut entry = Entry {
        id: id.clone(),
        options: Options {
            title: Text::parse("Open media").unwrap(),
            modal: true,
            mode: Mode::File,
            name: None,
            filter: None,
        },
        active: true,
        retired: false,
        report: Some(Report {
            outcome: Outcome::Selected {
                uri: Text::parse(uri).unwrap(),
            },
            portal_retired: true,
        }),
        selection: None,
        cancelled: Arc::new(AtomicBool::new(false)),
        slot: None,
    };
    assert_eq!(projection(&entry)["status"], "cleanup_pending");
    entry.active = false;
    assert_eq!(projection(&entry)["status"], "cleanup_pending");
    entry.retired = true;
    let result = projection(&entry);
    assert_eq!(result["uri"], uri);
    assert_eq!(result.as_object().unwrap().len(), 4);
    let request = json!({"session":"window","action":"open","request_id":id,"title":"Open media","modal":true});
    for field in [
        "owner_uid",
        "program",
        "bus",
        "path",
        "caps",
        "approved",
        "grant",
    ] {
        let mut invalid = request.clone();
        invalid[field] = json!("caller-selected");
        assert!(
            serde_json::from_value::<Request>(invalid).is_err(),
            "{field}"
        );
    }
}

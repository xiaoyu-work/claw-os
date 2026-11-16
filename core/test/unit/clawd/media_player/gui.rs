use super::*;

#[test]
fn playback_reply_cannot_change_the_admitted_action_or_disclose_unbounded_metadata() {
    let status = json!({
        "kind":"status", "status":"Playing", "title":"Native track", "album":null,
        "url":"file:///home/fixture/track.webm", "artist":["Native artist"], "length_micros":1000000,
    });
    let result: ResultValue = serde_json::from_value(status.clone()).unwrap();
    let value = result.project(MediaPlayerAction::Status).unwrap();
    assert_eq!(value["status"], "Playing");
    assert_eq!(value["title"], "Native track");
    assert!(value.get("kind").is_none());
    for action in [
        MediaPlayerAction::Play,
        MediaPlayerAction::Pause,
        MediaPlayerAction::Stop,
        MediaPlayerAction::Next,
        MediaPlayerAction::Previous,
        MediaPlayerAction::Toggle,
    ] {
        assert!(serde_json::from_value::<ResultValue>(status.clone())
            .unwrap()
            .project(action)
            .is_err());
        assert_eq!(
            ResultValue::Applied.project(action).unwrap(),
            json!({"ok":true})
        );
    }
    assert!(ResultValue::Applied
        .project(MediaPlayerAction::Status)
        .is_err());
    let mut large = status.clone();
    large["artist"] = json!(vec!["x".repeat(1024); 64]);
    assert!(serde_json::from_value::<ResultValue>(large)
        .unwrap()
        .project(MediaPlayerAction::Status)
        .is_err());
    for (field, invalid) in [
        ("status", json!("Approved")),
        ("title", json!("a".repeat(4097))),
        ("artist", json!(vec!["artist"; 65])),
        ("owner_uid", json!(0)),
        ("caps", json!(["desktop.media.control:*"])),
    ] {
        let mut value = status.clone();
        value[field] = invalid;
        assert!(
            serde_json::from_value::<ResultValue>(value).is_err(),
            "{field}"
        );
    }
}

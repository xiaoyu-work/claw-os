use super::*;

#[test]
fn video_nodes_require_video_source_class() {
    let node = json!({
        "id": 42,
        "type": "PipeWire:Interface:Node",
        "info": {"props": {"media.class": "Video/Source", "object.serial": 100}}
    });
    assert_eq!(normalize_video_node(&node).unwrap()["id"], 42);
    let audio = json!({
        "id": 43,
        "type": "PipeWire:Interface:Node",
        "info": {"props": {"media.class": "Audio/Source"}}
    });
    assert!(normalize_video_node(&audio).is_none());
}

#[test]
fn capture_dimensions_are_bounded() {
    validate_action(
        "capture",
        Some(42),
        Some(100),
        Some("/tmp/x.png"),
        Some("png"),
        1280,
        720,
    )
    .unwrap();
    assert!(validate_action(
        "capture",
        Some(42),
        Some(100),
        Some("/tmp/x.png"),
        Some("png"),
        9000,
        720,
    )
    .is_err());
}

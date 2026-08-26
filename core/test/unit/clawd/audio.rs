use super::*;

#[test]
fn volume_parser_handles_mute() {
    let value = parse_volume("Volume: 0.42 [MUTED]\n").unwrap();
    assert_eq!(value["percent"], 42.0);
    assert_eq!(value["muted"], true);
}

#[test]
fn action_validation_is_bounded() {
    validate_action("output-volume", None, Some("150")).unwrap();
    assert!(validate_action("output-volume", None, Some("151")).is_err());
    validate_action("input-mute", None, Some("toggle")).unwrap();
    assert!(validate_action("profile", Some("0"), Some("1")).is_err());
}

#[test]
fn inspect_properties_are_normalized() {
    let properties = parse_inspect_properties(
        "id 42, type PipeWire:Interface:Node/3\n  * media.class = \"Audio/Sink\"\n",
    );
    assert_eq!(properties["media.class"], "Audio/Sink");
}

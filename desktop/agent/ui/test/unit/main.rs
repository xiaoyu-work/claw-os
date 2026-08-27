use super::*;

#[test]
fn overlay_flags_publish_the_activation_payload() {
    let flags = Flags {
        overlay: true,
        activation: Some(OverlayActivation::default()),
        ..Flags::default()
    };
    assert!(flags.action().is_some());
}

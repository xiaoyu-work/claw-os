use super::*;

#[test]
fn activation_and_layer_close_reset_transient_overlay_state() {
    let mut state = OverlayState::new(false, None, false);
    state.begin_activation(OverlayActivation {
        voice: false,
        query: Some("ask".into()),
        context: Some("window".into()),
        context_file: None,
    });
    let generation = state.activation_generation();
    assert!(state.auto_submit());
    assert_eq!(state.pending_context(), Some("window"));

    state.begin_stream_context(true);
    state.consume_stream_context();
    assert_eq!(state.pending_context(), None);
    state.layer_done();
    assert!(!state.is_visible());
    assert!(state.activation_generation() > generation);
    assert!(!state.auto_submit());
}

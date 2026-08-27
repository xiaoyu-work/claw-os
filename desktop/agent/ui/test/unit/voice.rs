use super::*;

#[test]
fn stale_voice_completion_does_not_end_current_processing() {
    let mut state = VoiceState {
        generation: 8,
        phase: VoicePhase::Processing { generation: 8 },
        abort: None,
    };
    assert!(!state.finish(7));
    assert!(state.is_processing());
    assert!(state.finish(8));
    assert!(!state.is_active());
}

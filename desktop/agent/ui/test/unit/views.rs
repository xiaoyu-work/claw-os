use super::*;

#[test]
fn provider_label_prefers_bridge_label() {
    crate::localize::localize();
    let models = ModelsResponse {
        ready: true,
        provider: "anthropic".into(),
        model: "claude".into(),
        label: "Claude".into(),
        models: Vec::new(),
    };
    assert_eq!(App::provider_model_label(&models), "Claude");
}

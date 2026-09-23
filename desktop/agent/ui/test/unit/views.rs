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

#[test]
fn tool_metrics_use_compact_human_readable_units() {
    assert_eq!(format_latency(42), "42 ms");
    assert_eq!(format_latency(1_250), "1.2 s");
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1_536), "1.5 KiB");
    assert_eq!(format_bytes(2 * 1_024 * 1_024), "2.0 MiB");
}

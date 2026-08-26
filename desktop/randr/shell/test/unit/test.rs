use super::*;
use kdl::KdlDocument;

#[test]
fn test_kdl_serialization_deserialization() {
    let mut list = List::default();

    let mode1 = Mode {
        size: (1920, 1080),
        refresh_rate: 60000,
        preferred: true,
    };
    let mode2 = Mode {
        size: (1280, 720),
        refresh_rate: 60000,
        preferred: false,
    };

    let mode1_key = list.modes.insert(mode1);
    let mode2_key = list.modes.insert(mode2);

    let output = Output {
        serial_number: String::new(),
        name: "HDMI-A-1".to_string(),
        enabled: true,
        mirroring: Some("eDP-1".to_string()),
        make: Some("Hello".to_string()),
        model: "Hi".to_string(),
        physical: (344, 194),
        position: (0, 0),
        scale: 1.0,
        transform: Some(Transform::Normal),
        modes: vec![mode1_key, mode2_key],
        current: Some(mode1_key),
        adaptive_sync: Some(AdaptiveSyncState::Auto),
        adaptive_sync_availability: Some(AdaptiveSyncAvailability::Supported),
        xwayland_primary: Some(true),
    };

    list.outputs.insert(output);

    // Serialize to KDL
    let kdl_doc: KdlDocument = list.clone().into();
    let kdl_string = kdl_doc.to_string();

    // Parse back from KDL
    let parsed_doc: KdlDocument = kdl_string.parse().expect("KDL parse failed");
    let parsed_list = List::try_from(parsed_doc)
        .map_err(|e| {
            for err in &e.errors {
                eprintln!("{:?}", err);
            }
            e
        })
        .expect("KDL deserialization failed");

    // Compare the original and parsed List
    // Since SlotMap keys are not preserved, compare the Output fields and Mode values
    let orig_output = list.outputs.values().next().unwrap();
    let parsed_output = parsed_list.outputs.values().next().unwrap();

    assert_eq!(orig_output.serial_number, parsed_output.serial_number);
    assert_eq!(orig_output.name, parsed_output.name);
    assert_eq!(orig_output.enabled, parsed_output.enabled);
    assert_eq!(orig_output.mirroring, parsed_output.mirroring);
    assert_eq!(orig_output.make, parsed_output.make);
    assert_eq!(orig_output.model, parsed_output.model);
    assert_eq!(orig_output.physical, parsed_output.physical);
    assert_eq!(orig_output.position, parsed_output.position);
    assert_eq!(orig_output.scale, parsed_output.scale);
    assert_eq!(orig_output.transform, parsed_output.transform);
    assert_eq!(orig_output.adaptive_sync, parsed_output.adaptive_sync);
    assert_eq!(
        orig_output.adaptive_sync_availability,
        parsed_output.adaptive_sync_availability
    );
    assert_eq!(orig_output.xwayland_primary, parsed_output.xwayland_primary);

    // Compare modes by value (order should be preserved)
    let orig_modes: Vec<_> = orig_output.modes.iter().map(|k| &list.modes[*k]).collect();
    let parsed_modes: Vec<_> = parsed_output
        .modes
        .iter()
        .map(|k| &parsed_list.modes[*k])
        .collect();
    assert_eq!(orig_modes.len(), parsed_modes.len());
    for (a, b) in orig_modes.iter().zip(parsed_modes.iter()) {
        assert_eq!(a.size, b.size);
        assert_eq!(a.refresh_rate, b.refresh_rate);
        assert_eq!(a.preferred, b.preferred);
    }
}

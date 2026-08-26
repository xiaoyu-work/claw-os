use super::*;

#[test]
fn test_adapter_device_with_intermediary_state() {
    let mut adapter = Adapter {
        alias: "foo".to_owned(),
        address: "AA:BB:CC:DD:EE:FF".to_owned(),
        scanning: Active::Disabled,
        enabled: Active::Disabled,
    };
    adapter.update(vec![
        AdapterUpdate::Enabled(Active::Enabled),
        AdapterUpdate::Alias("xxx".to_owned()),
    ]);
    assert_eq!(adapter.enabled, Active::Enabled);
    assert_eq!(&adapter.alias, "xxx");

    adapter.enabled = Active::Disabling;
    adapter.update(vec![
        AdapterUpdate::Enabled(Active::Enabled),
        AdapterUpdate::Alias("xxx".to_owned()),
    ]);
    assert_eq!(adapter.enabled, Active::Disabling);

    adapter.scanning = Active::Enabling;
    adapter.update(vec![
        AdapterUpdate::Scanning(Active::Disabled),
        AdapterUpdate::Alias("xxx".to_owned()),
    ]);
    assert_eq!(adapter.scanning, Active::Enabling);

    adapter.update(vec![
        AdapterUpdate::Scanning(Active::Enabled),
        AdapterUpdate::Alias("xxx".to_owned()),
    ]);
    assert_eq!(adapter.scanning, Active::Enabled);
    assert_eq!(&adapter.alias, "xxx");
}

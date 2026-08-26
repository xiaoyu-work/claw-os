use super::*;

#[test]
fn test_update_device_with_intermediary_state() {
    let mut device = Device {
        alias: None,
        adapter: OwnedObjectPath::try_from("/dev/bluez/hci0").unwrap(),
        address: "AA:BB:CC:DD:EE:FF".to_owned(),
        enabled: Active::Disabled,
        paired: false,
        icon: "bluetooth-symbolic",
        battery: None,
    };
    device.update(vec![
        DeviceUpdate::Enabled(Active::Enabled),
        DeviceUpdate::Alias(Some("Foo".to_owned())),
    ]);
    assert_eq!(device.enabled, Active::Enabled);
    assert_eq!(device.alias, Some("Foo".to_owned()));

    device.enabled = Active::Disabling;
    device.update(vec![
        DeviceUpdate::Enabled(Active::Enabled),
        DeviceUpdate::Alias(Some("Foo".to_owned())),
    ]);
    assert_eq!(device.enabled, Active::Disabling);

    device.enabled = Active::Enabling;
    device.update(vec![
        DeviceUpdate::Enabled(Active::Enabled),
        DeviceUpdate::Alias(Some("Foo".to_owned())),
    ]);
    assert_eq!(device.enabled, Active::Enabled);
}

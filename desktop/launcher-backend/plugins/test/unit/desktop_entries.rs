use super::is_settings_entry;

#[test]
fn settings_category_uses_app_id_or_desktop_metadata() {
    assert!(is_settings_entry("com.clawos.Settings", &["ClawOS"]));
    assert!(is_settings_entry(
        "com.clawos.Settings.Appearance",
        &["ClawOS"]
    ));
    assert!(is_settings_entry(
        "org.example.ControlCenter",
        &["Settings"]
    ));
    assert!(!is_settings_entry("org.example.Editor", &["Utility"]));
}

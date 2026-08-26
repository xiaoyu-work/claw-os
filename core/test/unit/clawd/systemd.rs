use super::*;

#[test]
fn accepts_normal_and_template_units() {
    for unit in [
        "ssh.service",
        "user@1000.service",
        "apt-daily.timer",
        "home.mount",
    ] {
        validate_unit_name(unit).unwrap();
    }
}

#[test]
fn rejects_options_paths_and_unknown_suffixes() {
    for unit in [
        "",
        "--user",
        "../ssh.service",
        "ssh",
        "bad unit.service",
        "x.service/../../",
    ] {
        assert!(validate_unit_name(unit).is_err(), "{unit:?} should fail");
    }
}

#[test]
fn parses_systemctl_properties() {
    let values = parse_properties(
        "Id=demo.service\nDescription=Demo\nLoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\n",
    );
    let state = state_from_properties("demo.service", &values);
    assert!(state.active);
    assert_eq!(state.enabled, Some(true));
    assert_eq!(state.description, "Demo");
}

#[test]
fn static_units_have_no_enable_inverse() {
    assert_eq!(enabled_bool("static"), None);
    assert_eq!(enabled_bool("disabled"), Some(false));
}

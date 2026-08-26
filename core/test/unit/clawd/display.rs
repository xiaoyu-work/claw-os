use super::*;

#[test]
fn parses_cosmic_randr_kdl() {
    let outputs = parse_outputs(
        "output \"eDP-1\" enabled=#true {\n  position 0 0\n  scale 1.00\n  modes {\n    mode 1920 1080 60000 current=#true preferred=#true\n  }\n}\n",
    )
    .unwrap();
    assert_eq!(outputs[0]["name"], "eDP-1");
    assert_eq!(outputs[0]["modes"][0]["refresh_hz"], 60.0);
}

#[test]
fn refuses_last_output_disable_in_validation_inputs() {
    validate_action(
        "disable",
        Some("eDP-1"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();
}

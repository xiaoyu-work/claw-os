use super::*;

#[test]
fn parse() {
    let tests = [
        ("#ff0000", [255, 0, 0, 255], "#ff0000"),
        ("00ff0080", [0, 255, 0, 128], "#00ff0080"),
        ("#F80", [255, 136, 0, 255], "#ff8800"),
        ("#00f1", [0, 0, 255, 17], "#0000ff11"),
        ("#00ff", [0, 0, 255, 255], "#0000ff"),
    ];

    for (arg, expected_rgba8, expected_str) in tests {
        let color = arg.parse::<Color>().expect("color must parse");

        assert_eq!(color.into_rgba8(), expected_rgba8);
        assert_eq!(color.to_string(), expected_str);
    }

    assert!("invalid".parse::<Color>().is_err());
}

const SHORTHAND: Color = color!(0x123);

#[test]
fn shorthand_notation() {
    assert_eq!(SHORTHAND, Color::from_rgb8(0x11, 0x22, 0x33));
}

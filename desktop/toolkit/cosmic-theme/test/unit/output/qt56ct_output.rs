use super::*;

#[test]
fn test_color_to_argb_hex() {
    let color = Srgba::new(0x33, 0x55, 0x77, 0xff);
    let argb = to_argb_hex(color.into());
    assert_eq!(argb, "#ff335577");
}

#[test]
fn test_light_default_qpalette() {
    let light_default_qpalette = Theme::light_default().as_qpalette();
    insta::assert_snapshot!(light_default_qpalette);
}

#[test]
fn test_dark_default_qpalette() {
    let dark_default_qpalette = Theme::dark_default().as_qpalette();
    insta::assert_snapshot!(dark_default_qpalette);
}

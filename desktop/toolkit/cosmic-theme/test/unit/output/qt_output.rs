use super::*;

#[test]
fn test_opaque_color_to_rgb() {
    let color = Srgba::new(30.0 / 255.0, 50.0 / 255.0, 70.0 / 255.0, 1.0);
    let bg = Srgba::new(1.0, 1.0, 1.0, 1.0);
    let result = to_rgb(color, bg);
    assert_eq!(result, "30,50,70");
}

#[test]
fn test_transparent_color_to_rgb() {
    let color = Srgba::new(0.0, 0.0, 0.0, 0.0);
    let bg = Srgba::new(1.0, 1.0, 1.0, 1.0);
    let result = to_rgb(color, bg);
    assert_eq!(result, "255,255,255");
}

#[test]
fn test_translucent_color_to_rgb() {
    let color = Srgba::new(0.0, 0.0, 0.0, 0.9);
    let bg = Srgba::new(1.0, 1.0, 1.0, 1.0);
    let result = to_rgb(color, bg);
    assert_eq!(result, "26,26,26");
}

#[test]
fn test_light_default_kcolorscheme() {
    let light_default_kcolorscheme = Theme::light_default().as_kcolorscheme();
    insta::assert_snapshot!(light_default_kcolorscheme);
}

#[test]
fn test_dark_default_kcolorscheme() {
    let dark_default_kcolorscheme = Theme::dark_default().as_kcolorscheme();
    insta::assert_snapshot!(dark_default_kcolorscheme);
}

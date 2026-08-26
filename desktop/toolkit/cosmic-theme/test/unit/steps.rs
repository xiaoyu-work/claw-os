use palette::{OklabHue, Srgba};

use super::{is_valid_srgb, oklch_to_srgba_nearest_chroma};

#[test]
fn test_valid_check() {
    assert!(is_valid_srgb(Srgba::new(1.0, 1.0, 1.0, 1.0)));
    assert!(is_valid_srgb(Srgba::new(0.0, 0.0, 0.0, 1.0)));
    assert!(is_valid_srgb(Srgba::new(0.5, 0.5, 0.5, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(-0.1, 0.0, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(0.0, -0.1, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(-0.0, 0.0, -0.1, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(-100.1, 0.0, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(0.0, -100.1, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(-0.0, 0.0, -100.1, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(1.1, 0.0, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(0.0, 1.1, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(-0.0, 0.0, 1.1, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(100.1, 0.0, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(0.0, 100.1, 0.0, 1.0)));
    assert!(!is_valid_srgb(Srgba::new(-0.0, 0.0, 100.1, 1.0)));
}

#[test]
fn test_conversion_boundaries() {
    let c1 = palette::Oklcha::new(0.0, 0.288, OklabHue::from_degrees(0.0), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1);
    almost::zero(srgb.red);
    almost::zero(srgb.blue);
    almost::zero(srgb.green);

    let c1 = palette::Oklcha::new(1.0, 0.288, OklabHue::from_degrees(0.0), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1);

    almost::equal(srgb.red, 1.0);
    almost::equal(srgb.blue, 1.0);
    almost::equal(srgb.green, 1.0);
}

#[test]
fn test_conversion_colors() {
    let c1 = palette::Oklcha::new(0.4608, 0.11111, OklabHue::new(57.31), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1).into_format::<u8, u8>();
    assert_eq!(srgb.red, 133);
    assert_eq!(srgb.green, 69);
    assert_eq!(srgb.blue, 0);

    let c1 = palette::Oklcha::new(0.30, 0.08, OklabHue::new(35.0), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1).into_format::<u8, u8>();
    assert_eq!(srgb.red, 78);
    assert_eq!(srgb.green, 27);
    assert_eq!(srgb.blue, 15);

    let c1 = palette::Oklcha::new(0.757, 0.146, OklabHue::new(301.2), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1).into_format::<u8, u8>();
    assert_eq!(srgb.red, 192);
    assert_eq!(srgb.green, 153);
    assert_eq!(srgb.blue, 253);
}

#[test]
fn test_conversion_fallback_colors() {
    let c1 = palette::Oklcha::new(0.70, 0.284, OklabHue::new(35.0), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1).into_format::<u8, u8>();
    assert_eq!(srgb.red, 255);
    assert_eq!(srgb.green, 102);
    assert_eq!(srgb.blue, 65);

    let c1 = palette::Oklcha::new(0.757, 0.239, OklabHue::new(301.2), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1).into_format::<u8, u8>();
    assert_eq!(srgb.red, 193);
    assert_eq!(srgb.green, 152);
    assert_eq!(srgb.blue, 255);

    let c1 = palette::Oklcha::new(0.163, 0.333, OklabHue::new(141.0), 1.0);
    let srgb = oklch_to_srgba_nearest_chroma(c1).into_format::<u8, u8>();
    assert_eq!(srgb.red, 1);
    assert_eq!(srgb.green, 19);
    assert_eq!(srgb.blue, 0);
}

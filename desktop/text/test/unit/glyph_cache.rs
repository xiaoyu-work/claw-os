#[test]
fn test_subpixel_bins() {
    // POSITIVE TESTS

    // Maps to 0.0
    assert_eq!(SubpixelBin::new(0.0), (0, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(0.124), (0, SubpixelBin::Zero));

    // Maps to 0.25
    assert_eq!(SubpixelBin::new(0.125), (0, SubpixelBin::One));
    assert_eq!(SubpixelBin::new(0.25), (0, SubpixelBin::One));
    assert_eq!(SubpixelBin::new(0.374), (0, SubpixelBin::One));

    // Maps to 0.5
    assert_eq!(SubpixelBin::new(0.375), (0, SubpixelBin::Two));
    assert_eq!(SubpixelBin::new(0.5), (0, SubpixelBin::Two));
    assert_eq!(SubpixelBin::new(0.624), (0, SubpixelBin::Two));

    // Maps to 0.75
    assert_eq!(SubpixelBin::new(0.625), (0, SubpixelBin::Three));
    assert_eq!(SubpixelBin::new(0.75), (0, SubpixelBin::Three));
    assert_eq!(SubpixelBin::new(0.874), (0, SubpixelBin::Three));

    // Maps to 1.0
    assert_eq!(SubpixelBin::new(0.875), (1, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(0.999), (1, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(1.0), (1, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(1.124), (1, SubpixelBin::Zero));

    // NEGATIVE TESTS

    // Maps to 0.0
    assert_eq!(SubpixelBin::new(-0.0), (0, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(-0.124), (0, SubpixelBin::Zero));

    // Maps to 0.25
    assert_eq!(SubpixelBin::new(-0.125), (-1, SubpixelBin::Three));
    assert_eq!(SubpixelBin::new(-0.25), (-1, SubpixelBin::Three));
    assert_eq!(SubpixelBin::new(-0.374), (-1, SubpixelBin::Three));

    // Maps to 0.5
    assert_eq!(SubpixelBin::new(-0.375), (-1, SubpixelBin::Two));
    assert_eq!(SubpixelBin::new(-0.5), (-1, SubpixelBin::Two));
    assert_eq!(SubpixelBin::new(-0.624), (-1, SubpixelBin::Two));

    // Maps to 0.75
    assert_eq!(SubpixelBin::new(-0.625), (-1, SubpixelBin::One));
    assert_eq!(SubpixelBin::new(-0.75), (-1, SubpixelBin::One));
    assert_eq!(SubpixelBin::new(-0.874), (-1, SubpixelBin::One));

    // Maps to 1.0
    assert_eq!(SubpixelBin::new(-0.875), (-1, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(-0.999), (-1, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(-1.0), (-1, SubpixelBin::Zero));
    assert_eq!(SubpixelBin::new(-1.124), (-1, SubpixelBin::Zero));
}

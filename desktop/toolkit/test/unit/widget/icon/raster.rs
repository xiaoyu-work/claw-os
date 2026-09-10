use super::*;

fn pixels(handle: &Handle) -> &[u8] {
    let Handle::Rgba { pixels, .. } = handle else {
        panic!("expected decoded RGBA")
    };
    pixels.as_ref()
}

#[test]
fn symbolic_pixels_keep_alpha_and_follow_the_inherited_icon_color() {
    let source = Handle::from_rgba(3, 1, vec![1, 2, 3, 0, 4, 5, 6, 127, 7, 8, 9, 255]);
    let mut cache = Cache::default();
    let dark = cache.handle(&source, [230, 231, 232, 255]);
    assert_eq!(
        pixels(&dark),
        &[1, 2, 3, 0, 230, 231, 232, 127, 230, 231, 232, 255]
    );
    assert_eq!(dark.id(), cache.handle(&source, [230, 231, 232, 255]).id());
    let light = cache.handle(&source, [10, 11, 12, 255]);
    assert_eq!(
        pixels(&light),
        &[1, 2, 3, 0, 10, 11, 12, 127, 10, 11, 12, 255]
    );
    assert_ne!(dark.id(), light.id());
    assert_eq!(pixels(&source), &[1, 2, 3, 0, 4, 5, 6, 127, 7, 8, 9, 255]);
}

#[test]
fn new_pixel_sources_invalidate_the_render_cache_without_decoding() {
    let mut cache = Cache::default();
    let first = cache.handle(&Handle::from_rgba(1, 1, vec![0, 0, 0, 10]), [1, 2, 3, 255]);
    let second = cache.handle(&Handle::from_rgba(1, 1, vec![0, 0, 0, 20]), [1, 2, 3, 255]);
    assert_ne!(first.id(), second.id());
    assert_eq!(pixels(&second), &[1, 2, 3, 20]);
}

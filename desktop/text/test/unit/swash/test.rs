use super::*;
use swash::{FontRef, Setting, Tag};

// variations() resizes context.coords in place (stale values persist),
// whereas using normalized_coords() clears and replaces them.
#[test]
fn no_coord_leakage_across_fonts() {
    let [Ok(sfns), Ok(sfns_italic)] = [
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/SFNSItalic.ttf",
    ]
    .map(std::fs::read) else {
        return;
    };
    let regular = FontRef::from_index(&sfns, 0).unwrap();
    let italic = FontRef::from_index(&sfns_italic, 0).unwrap();
    let wght = Tag::from_be_bytes(*b"wght");

    let render = |ctx: &mut ScaleContext, font: FontRef, weight: f32, use_normalized| {
        let mut b = ctx.builder(font).size(16.0).hint(true);
        if use_normalized {
            b = b.normalized_coords(font.variations().normalized_coords([(wght, weight)]));
        } else {
            b = b.variations(std::iter::once(Setting {
                tag: wght,
                value: weight,
            }));
        }
        Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .render(&mut b.build(), 36)
    };

    // reference: regular@400 with no prior context
    let mut ctx = ScaleContext::new();
    let reference = render(&mut ctx, regular, 400.0, false).map(|i| i.data);

    // variations(): pollute ctx with italic@700, then render regular@400
    let mut ctx = ScaleContext::new();
    render(&mut ctx, italic, 700.0, false);
    let not_normalized = render(&mut ctx, regular, 400.0, false).map(|i| i.data);

    // normalized_coords(): same sequence
    let mut ctx = ScaleContext::new();
    render(&mut ctx, italic, 700.0, true);
    let normalized = render(&mut ctx, regular, 400.0, true).map(|i| i.data);

    assert_ne!(not_normalized, reference, "variations leak across fonts");
    assert_eq!(
        normalized, reference,
        "normalized_coords match clean render"
    );
}

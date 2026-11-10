use super::*;
use claw_notification_presentation::VERSION;

fn card() -> Card {
    Card {
        version: VERSION,
        id: 7,
        source_label: "Display label, not identity".into(),
        title: "Title".into(),
        body: vec![TextRun {
            text: "<not parsed as markup>".into(),
            bold: true,
            italic: true,
            underline: true,
            accent: true,
        }],
        activation: Some("default".into()),
        icon: Some(Icon::Rgba {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 255],
        }),
        group_icon: Some(Icon::Mask {
            width: 2,
            height: 1,
            alpha: vec![0, 127],
        }),
    }
}

#[test]
fn popup_mute_requires_confirmation_and_preserves_last_value_on_failure_or_disconnect() {
    let mut state = PreferenceState::default();
    assert!(!state.begin(true));
    state.observe(Preferences::new(false));
    assert!(!state.begin(true));
    state.connected = true;
    assert!(state.begin(true));
    assert!(!state.muted());
    assert!(!state.enabled());
    state.finish(Err("write denied".into()));
    assert!(!state.muted());
    assert!(state.enabled());
    assert_eq!(state.error.as_deref(), Some("write denied"));
    assert!(state.begin(true));
    state.finish(Ok(Preferences::new(true)));
    assert!(state.muted());
    state.disconnect("presenter closed".into());
    assert!(state.muted());
    assert!(!state.enabled());
    assert!(!state.begin(false));
}

#[test]
fn rendering_preserves_text_runs_and_independent_row_and_symbolic_group_icons() {
    let value = PresentedCard::new(card()).unwrap();
    assert_eq!(value.card.activation.as_deref(), Some("default"));
    let runs = spans(&value.card.body);
    assert_eq!(runs[0].text, "<not parsed as markup>");
    assert_eq!(runs[0].font.unwrap().weight, Weight::Bold);
    assert_eq!(runs[0].font.unwrap().style, Style::Italic);
    assert!(runs[0].underline);
    assert!(runs[0].color.is_some());
    let row = value.icon.unwrap();
    assert!(!row.symbolic);
    let group = value.group_icon.unwrap();
    assert!(group.symbolic);
    let icon::Data::Image(cosmic::widget::image::Handle::Rgba {
        width,
        height,
        pixels,
        ..
    }) = group.data
    else {
        panic!("decoded symbolic mask must not become an encoded image or resource lookup");
    };
    assert_eq!((width, height), (2, 1));
    assert_eq!(pixels.as_ref(), &[255, 255, 255, 0, 255, 255, 255, 127]);
}

#[test]
fn invalid_image_work_is_rejected_before_render_handle_creation() {
    let mut value = card();
    value.group_icon = Some(Icon::Mask {
        width: 513,
        height: 1,
        alpha: vec![255],
    });
    assert!(PresentedCard::new(value).is_err());
}

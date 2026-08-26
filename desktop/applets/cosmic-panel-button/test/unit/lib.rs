use super::*;

#[test]
fn divider_beats_vertical_dock_icon_override() {
    assert_eq!(
        presentation_for(
            &PanelAnchor::Left,
            &Size::PanelSize(PanelSize::M),
            true,
            Some(Override::Icon),
            Some(Override::Divider),
        ),
        Override::Divider,
    );
}

#[test]
fn per_entry_brand_presentation_is_used_on_top_panel() {
    assert_eq!(
        presentation_for(
            &PanelAnchor::Top,
            &Size::PanelSize(PanelSize::XS),
            true,
            None,
            Some(Override::IconAndText),
        ),
        Override::IconAndText,
    );
}

#[test]
fn vertical_panels_remain_icon_only() {
    assert_eq!(
        presentation_for(
            &PanelAnchor::Left,
            &Size::PanelSize(PanelSize::M),
            true,
            None,
            Some(Override::IconAndText),
        ),
        Override::Icon,
    );
}

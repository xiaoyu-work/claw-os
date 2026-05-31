//! Shared "Claw Glass" selection treatment for wizard list rows.
//!
//! Selectable rows (language, keyboard, timezone, …) must read as brand-blue
//! Claw Glass, never gray. A selected row gets a brand-blue translucent fill
//! plus an accent hairline and accent label; idle rows are transparent and
//! pick up a faint brand-blue wash on hover/press. This replaces the older
//! blue-text-only `Button::Link` / `Button::MenuRoot` pairing so the whole row
//! highlights, matching the analyzed onboarding references.

use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::Background;
use cosmic::theme;
use cosmic::widget::button::Style;

/// Translucency steps for the brand-blue selection treatment.
const SELECTED_FILL_ALPHA: f32 = 0.15;
const SELECTED_BORDER_ALPHA: f32 = 0.50;
const HOVER_FILL_ALPHA: f32 = 0.08;

/// Returns a `theme::Button` class that renders a wizard list row in the
/// Claw Glass selection style. Pass whether the row is currently selected.
pub fn list_row(selected: bool) -> theme::Button {
    theme::Button::Custom {
        active: Box::new(move |_focused, theme| row_style(theme, selected, false)),
        hovered: Box::new(move |_focused, theme| row_style(theme, selected, true)),
        pressed: Box::new(move |_focused, theme| row_style(theme, selected, true)),
        disabled: Box::new(move |theme| row_style(theme, selected, false)),
    }
}

fn row_style(theme: &cosmic::Theme, selected: bool, hovered: bool) -> Style {
    let cosmic = theme.cosmic();
    let accent = cosmic.accent_color();

    let mut style = Style::new();
    // Controls use radius_m (10) per the Claw Glass spec.
    style.border_radius = cosmic.radius_m().into();

    if selected {
        style.background = Some(Background::Color(accent.with_alpha(SELECTED_FILL_ALPHA).into()));
        style.border_width = 1.0;
        style.border_color = accent.with_alpha(SELECTED_BORDER_ALPHA).into();
        style.text_color = Some(cosmic.accent_text_color().into());
        style.icon_color = Some(cosmic.accent_text_color().into());
    } else {
        if hovered {
            style.background = Some(Background::Color(accent.with_alpha(HOVER_FILL_ALPHA).into()));
        }
        style.text_color = Some(cosmic.on_bg_color().into());
        style.icon_color = Some(cosmic.on_bg_color().into());
    }

    style
}

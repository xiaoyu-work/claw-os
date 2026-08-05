//! Shared visual treatments for the portal dialogs.
//!
//! Centralizes the design-system tokens (frosted `radius_l` cards, neutral
//! hairlines, accent selection) so every system dialog - file chooser,
//! access/consent, screencast, screenshot - reads as the same frosted-glass
//! language instead of re-deriving styles inline.
//!
//! Chrome is neutral. The brand accent `#005CFE` is spent only on state, so
//! that a selected row is the one thing in the dialog wearing it.

use cosmic::iced::{Background, Border, Color};
use cosmic::theme;
use cosmic::widget::container;

/// Alpha for a resting accent selection fill.
pub const SELECTION_ALPHA: f32 = 0.16;

/// Brand accent as an `iced` color with the given alpha. Selection only.
pub fn accent_alpha(theme: &cosmic::Theme, alpha: f32) -> Color {
    let mut color: Color = theme.cosmic().accent_color().into();
    color.a = alpha;
    color
}

/// Neutral hairline for chrome.
///
/// `on_bg_color` is near-black on a light theme and near-white on a dark one,
/// so a low alpha of it separates surfaces in both without tinting them.
pub fn hairline(theme: &cosmic::Theme) -> Color {
    let cosmic = theme.cosmic();
    let mut color: Color = cosmic.on_bg_color().into();
    color.a = if cosmic.is_dark { 0.15 } else { 0.09 };
    color
}

/// Frosted `radius_l` card: translucent component fill + 1px neutral hairline.
///
/// The compositor's blur supplies depth; the hairline only hints separation.
pub fn frosted_card() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        container::Style {
            background: Some(Background::Color(cosmic.bg_component_color().into())),
            border: Border {
                radius: cosmic.radius_l().into(),
                width: 1.0,
                color: hairline(theme),
            },
            ..Default::default()
        }
    })
}

/// Accent translucent selection row/tile (`radius_m`).
///
/// `selected` gives a resting accent fill and matching hairline; otherwise the
/// surface is transparent so hover styling can layer on top. This is the one
/// place in a portal dialog that carries the brand colour, which is what makes
/// the selected item legible at a glance.
pub fn selection_tile(selected: bool) -> theme::Container<'static> {
    theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        let (background, width, color) = if selected {
            (
                Some(Background::Color(accent_alpha(theme, SELECTION_ALPHA))),
                1.0,
                accent_alpha(theme, 0.34),
            )
        } else {
            (None, 0.0, Color::TRANSPARENT)
        };
        container::Style {
            background,
            border: Border {
                radius: cosmic.radius_m().into(),
                width,
                color,
            },
            ..Default::default()
        }
    })
}

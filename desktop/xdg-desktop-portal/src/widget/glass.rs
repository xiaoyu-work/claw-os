//! Claw Glass shared visual treatments for the portal dialogs.
//!
//! Centralizes the design-system tokens (frosted `radius_l` cards, blue
//! hairlines, brand-blue translucent selection) so every system dialog —
//! file chooser, access/consent, screencast, screenshot — reads as the same
//! frosted-glass language instead of re-deriving styles inline.
//!
//! Brand accent is Claw blue `#005CFE`; surfaces are never flat gray.

use cosmic::iced::{Background, Border, Color};
use cosmic::theme;
use cosmic::widget::container;

/// Alpha for blue hairline borders (1px translucent accent).
pub const HAIRLINE_ALPHA: f32 = 0.20;
/// Alpha for a resting brand-blue translucent selection fill.
pub const SELECTION_ALPHA: f32 = 0.16;

/// Brand-blue accent as an `iced` color with the given alpha.
pub fn accent_alpha(theme: &cosmic::Theme, alpha: f32) -> Color {
    let mut color: Color = theme.cosmic().accent_color().into();
    color.a = alpha;
    color
}

/// Frosted `radius_l` card: translucent component fill + 1px blue hairline.
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
                color: accent_alpha(theme, HAIRLINE_ALPHA),
            },
            ..Default::default()
        }
    })
}

/// Brand-blue translucent selection row/tile (`radius_m`), never gray.
///
/// `selected` gives a resting accent fill + accent hairline; otherwise the
/// surface is transparent so hover styling can layer on top.
pub fn selection_tile(selected: bool) -> theme::Container<'static> {
    theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        let (background, width) = if selected {
            (
                Some(Background::Color(accent_alpha(theme, SELECTION_ALPHA))),
                1.0,
            )
        } else {
            (None, 0.0)
        };
        container::Style {
            background,
            border: Border {
                radius: cosmic.radius_m().into(),
                width,
                color: accent_alpha(theme, HAIRLINE_ALPHA),
            },
            ..Default::default()
        }
    })
}

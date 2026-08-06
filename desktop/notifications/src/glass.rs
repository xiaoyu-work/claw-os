// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Claw Glass — surface treatments for the notifications daemon toasts.
//!
//! The shared toolkit/theme foundation provides blue-tinted glass colors, Inter
//! typography and brand-blue accents for free. These helpers add the
//! *structural* Claw Glass treatment the daemon is responsible for: a frosted
//! notification card at `radius_l` (16) with a translucent component fill, a 1px
//! blue-tinted hairline and a soft downward drop shadow — depth from blur +
//! shadow, not heavy borders.
//!
//! Note: the toolkit's stacked `cards` widget draws no per-card background in
//! the expanded (multi-toast) state, so wrapping each notification's inner
//! element in [`card_class`] makes our frosted surface the visible toast while
//! preserving the widget's activation / dismiss behavior.

use cosmic::{
    iced::{Background, Border, Color, Shadow, Vector, widget::container},
    theme,
};

/// Neutral hairline for glass chrome.
///
/// `on_bg_color` is near-black on a light theme and near-white on a dark one,
/// so a low alpha of it separates the card from the wallpaper in both without
/// tinting the glass.
fn hairline(theme: &cosmic::Theme) -> Color {
    let cosmic = theme.cosmic();
    let mut color: Color = cosmic.on_bg_color().into();
    color.a = if cosmic.is_dark { 0.15 } else { 0.09 };
    color
}

/// Frosted glass notification card: `radius_l` (16) corners, translucent
/// component fill, a 1px blue-tinted hairline and a soft drop shadow. This is
/// the Claw Glass treatment for a single notification toast.
pub fn card_class() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();

        // Translucent component surface so the compositor's blur reads
        // through. bg_component_color is the spec-recommended toast fill.
        let mut background: Color = cosmic.bg_component_color().into();
        background.a = 0.60;

        let on: Color = cosmic.on_bg_color().into();

        container::Style {
            icon_color: Some(on),
            text_color: Some(on),
            background: Some(Background::Color(background)),
            border: Border {
                radius: cosmic.radius_l().into(),
                width: 1.0,
                color: hairline(theme),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.18),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 24.0,
            },
            snap: true,
        }
    })
}

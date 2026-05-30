// Copyright 2024 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Claw Glass app-level styling for the Media Player.
//!
//! Shared frosted-glass container treatments used by the player chrome:
//! the floating transport bar, the now-playing album-art card, and the
//! audio/subtitle/volume popovers. The shared theme + toolkit already supply
//! the brand-blue accent (scrubber fill, focus rings, selection), Inter type,
//! and corner radii; these helpers only apply the app-level structure: frosted
//! translucent surfaces, blue-tinted hairlines, and soft layered shadows.

use cosmic::iced::{Background, Border, Color, Shadow, Vector};
use cosmic::theme;
use cosmic::widget::container;

/// Mix the brand-blue accent into a hairline/border color at the given alpha,
/// so separators read as cool glass rather than neutral gray.
fn accent_hairline(theme: &theme::Theme, alpha: f32) -> Color {
    let accent = theme.cosmic().accent.base;
    Color {
        r: accent.red,
        g: accent.green,
        b: accent.blue,
        a: alpha,
    }
}

/// Floating transport / control bar: a frosted glass slab that hovers over the
/// video (or under the now-playing panel). Rounded `radius_l` (16), a 1px
/// blue-tinted hairline, and a soft drop shadow give it depth without heavy
/// borders — the compositor blur does the rest.
pub fn control_bar() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let corners = cosmic.corner_radii;

        // Translucent navy/blue-white glass — bright, never flat charcoal.
        let mut bg: Color = cosmic.background.base.into();
        bg.a = 0.82;

        container::Style {
            icon_color: Some(cosmic.background.on.into()),
            text_color: Some(cosmic.background.on.into()),
            background: Some(Background::Color(bg)),
            border: Border {
                radius: corners.radius_l.into(),
                width: 1.0,
                color: accent_hairline(theme, 0.16),
            },
            shadow: Shadow {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.28,
                },
                offset: Vector { x: 0.0, y: 8.0 },
                blur_radius: 28.0,
            },
            snap: true,
        }
    })
}

/// Now-playing album-art card: a frosted `radius_l` container that frames the
/// artwork (or fallback icon) with a faint blue hairline and soft shadow.
pub fn now_playing_card() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let corners = cosmic.corner_radii;

        let mut bg: Color = cosmic.background.component.base.into();
        bg.a = 0.55;

        container::Style {
            icon_color: Some(cosmic.background.component.on.into()),
            text_color: Some(cosmic.background.component.on.into()),
            background: Some(Background::Color(bg)),
            border: Border {
                radius: corners.radius_l.into(),
                width: 1.0,
                color: accent_hairline(theme, 0.14),
            },
            shadow: Shadow {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.30,
                },
                offset: Vector { x: 0.0, y: 10.0 },
                blur_radius: 32.0,
            },
            snap: true,
        }
    })
}

/// Audio / subtitle / volume popover surface: a frosted `radius_l` card with a
/// blue-tinted hairline and soft shadow (upgrades the old 8px gray popover to
/// 16px blue glass).
pub fn popover_card() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let corners = cosmic.corner_radii;

        let mut bg: Color = cosmic.background.base.into();
        bg.a = 0.90;

        container::Style {
            icon_color: Some(cosmic.background.on.into()),
            text_color: Some(cosmic.background.on.into()),
            background: Some(Background::Color(bg)),
            border: Border {
                radius: corners.radius_l.into(),
                width: 1.0,
                color: accent_hairline(theme, 0.18),
            },
            shadow: Shadow {
                color: Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.30,
                },
                offset: Vector { x: 0.0, y: 8.0 },
                blur_radius: 28.0,
            },
            snap: true,
        }
    })
}

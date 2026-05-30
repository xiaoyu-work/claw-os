// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Claw Glass — app-level surface treatments for the Store.
//!
//! The shared toolkit/theme foundation gives us blue-tinted glass colors,
//! Inter typography and brand-blue selection for free. These helpers add the
//! *structural* Claw Glass treatments this app is responsible for: frosted
//! cards at `radius_l` (16), an accent-tinted featured hero, and rounded
//! screenshot frames — with 1px blue-tinted hairlines and soft shadows instead
//! of heavy borders.
//!
//! Note: the toolkit's `Container::Card` uses `radius_s` (6); per the Claw Glass
//! spec large surfaces/cards must be `radius_l` (16), so the Store defines its
//! own glass card here rather than mutating the shared toolkit.

use cosmic::{
    iced::{Background, Border, Color, Shadow, Vector, widget::container},
    theme,
};

/// Faint blue-tinted translucent hairline derived from the brand accent.
fn accent_hairline(theme: &cosmic::Theme, alpha: f32) -> Color {
    let mut color: Color = theme.cosmic().accent_color().into();
    color.a = alpha;
    color
}

/// Frosted glass card: `radius_l` (16) corners, translucent component fill, a
/// 1px blue-tinted hairline and a soft drop shadow. This is the Claw Glass
/// treatment for app cards, grid items and grouped surfaces.
pub fn card_class() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let container = theme.current_container();
        let component = &container.component;

        let mut background: Color = component.base.into();
        background.a = 0.55;

        container_style(
            background,
            component.on.into(),
            accent_hairline(theme, 0.10),
            cosmic.corner_radii.radius_l,
            Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.10),
                offset: Vector::new(0.0, 2.0),
                blur_radius: 12.0,
            },
        )
    })
}

/// Accent-tinted featured hero: a brand-blue frosted spotlight surface used for
/// the Explore featured banner and the app-detail header. Reads as a glass card
/// carrying a faint Claw-blue cast with a slightly stronger accent hairline.
pub fn hero_class() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let container = theme.current_container();

        let mut background: Color = cosmic.accent_color().into();
        background.a = 0.12;

        container_style(
            background,
            container.on.into(),
            accent_hairline(theme, 0.22),
            cosmic.corner_radii.radius_l,
            Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.10),
                offset: Vector::new(0.0, 3.0),
                blur_radius: 18.0,
            },
        )
    })
}

/// Rounded frame for screenshots: `radius_l` glass surface with a hairline so
/// large preview images read as framed cards rather than bare bitmaps.
pub fn screenshot_class() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let container = theme.current_container();
        let component = &container.component;

        let mut background: Color = component.base.into();
        background.a = 0.45;

        container_style(
            background,
            component.on.into(),
            accent_hairline(theme, 0.12),
            cosmic.corner_radii.radius_l,
            Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.08),
                offset: Vector::new(0.0, 2.0),
                blur_radius: 10.0,
            },
        )
    })
}

fn container_style(
    background: Color,
    on: Color,
    hairline: Color,
    radius: [f32; 4],
    shadow: Shadow,
) -> container::Style {
    container::Style {
        icon_color: Some(on),
        text_color: Some(on),
        background: Some(Background::Color(background)),
        border: Border {
            radius: radius.into(),
            width: 1.0,
            color: hairline,
        },
        shadow,
        snap: true,
    }
}

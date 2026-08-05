// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    cosmic_theme::palette::WithAlpha,
    iced::{Background, Border, Color, Shadow, Vector},
    theme,
};

/// Claw Glass — a frosted, brand-blue-tinted grouped card.
///
/// Replaces the old boxed setting-row look with an airy `radius_l` (16)
/// surface, a 1px blue-tinted hairline, and a soft drop shadow. Depth comes
/// from elevation, not heavy borders.
///
/// The Settings window is opaque (unlike Files, it is not a transparent
/// blurred shell), so the fill is kept close to solid: a low alpha here does
/// not reveal a blurred wallpaper, it only washes the card out against the
/// window background and drops row contrast.
#[must_use]
pub fn frosted_card() -> cosmic::theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();

        // Card fill: the component surface, kept near-solid so the row reads
        // as a distinct raised element on the opaque window background.
        let mut background = cosmic.bg_component_color();
        background.alpha = if cosmic.is_high_contrast { 1.0 } else { 0.94 };

        // 1px blue-tinted hairline (brand accent at low alpha).
        let hairline = Color::from(cosmic.accent_color().with_alpha(0.12));

        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(Background::Color(background.into())),
            border: Border {
                color: hairline,
                radius: cosmic.corner_radii.radius_l.into(),
                width: 1.0,
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.05, 0.18, 0.10),
                offset: Vector::new(0.0, 2.0),
                blur_radius: 16.0,
            },
            snap: false,
        }
    })
}

/// Claw Glass — a rounded leading "icon tile" for list rows.
///
/// A `radius_m` (10) brand-blue-tinted glass square that frames a category
/// or row icon, tinting the glyph brand blue. Mirrors the iOS/macOS settings
/// "icon tile + label + chevron" row pattern.
#[must_use]
pub fn icon_tile() -> cosmic::theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let accent = cosmic.accent_color();

        cosmic::widget::container::Style {
            icon_color: Some(accent.into()),
            text_color: Some(accent.into()),
            background: Some(Background::Color(Color::from(accent.with_alpha(0.14)))),
            border: Border {
                color: Color::TRANSPARENT,
                radius: cosmic.corner_radii.radius_m.into(),
                width: 0.0,
            },
            shadow: Shadow::default(),
            snap: false,
        }
    })
}

#[must_use]
pub fn display_container_frame() -> cosmic::theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(cosmic::iced::Background::Color(cosmic::iced::Color::WHITE)),
            border: Border {
                color: cosmic::iced::Color::WHITE,
                radius: cosmic.corner_radii.radius_xs.into(),
                width: 3.0,
            },
            shadow: Default::default(),
            snap: true,
        }
    })
}

#[must_use]
pub fn display_container_screen() -> cosmic::theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(cosmic::iced::Background::Color(cosmic::iced::Color::BLACK)),
            border: Border {
                color: cosmic::iced::Color::BLACK,
                radius: cosmic.corner_radii.radius_0.into(),
                width: 0.0,
            },
            shadow: Default::default(),
            snap: true,
        }
    })
}

// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    cosmic_theme::palette::WithAlpha,
    iced::{Background, Border, Color, Shadow, Vector},
    theme,
};

/// Fill for an elevated Claw Glass card.
///
/// ClawOS light puts `component.base` (`#D7DAE1`) *below* the page background
/// (`#EEF1F8`), so raising its opacity sinks a card instead of lifting it.
/// Light mode therefore elevates towards white; dark mode keeps the component
/// surface, which is already the lighter of the two.
fn elevated_fill(cosmic: &cosmic::cosmic_theme::Theme) -> Color {
    if cosmic.is_dark {
        let mut base = cosmic.bg_component_color();
        base.alpha = if cosmic.is_high_contrast { 1.0 } else { 0.94 };
        Color::from(base)
    } else {
        Color::from(cosmic.palette.neutral_0)
    }
}

/// Claw Glass — a brand-blue-tinted grouped card.
///
/// Used for the category rows on the Settings landing page: an airy
/// `radius_l` surface, a 1px blue-tinted hairline, and a soft drop shadow.
/// Depth comes from elevation, not heavy borders.
#[must_use]
pub fn frosted_card() -> cosmic::theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();

        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(Background::Color(elevated_fill(cosmic))),
            border: Border {
                color: Color::from(cosmic.accent_color().with_alpha(0.12)),
                radius: cosmic.corner_radii.radius_l.into(),
                width: 1.0,
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.043, 0.18, 0.10),
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

/// Claw Glass — the grouped section card used inside settings pages.
///
/// The stock `Container::List` fills with `component.base` at 0.42, which on
/// ClawOS composites to `#E4E7EE` against the `#EEF1F8` page — barely 10/255
/// of separation, so sections read as a flat wash. This instead *elevates*
/// the card: in light mode it is brighter than the page (the macOS grouped
/// list direction), in dark mode it stays on the component surface, which is
/// already lighter than the dark page.
/// Claw Glass — the grouped section card used inside settings pages.
///
/// The stock `Container::List` fills with `component.base`, which on ClawOS
/// light composites to `#DDE0E7` against the `#EEF1F8` page and carries no
/// border or shadow — sections read as a flat wash. This instead *elevates*
/// the card: in light mode it is brighter than the page (the macOS grouped
/// list direction), in dark mode it stays on the component surface, which is
/// already lighter than the dark page.
///
/// The radius must stay `radius_s`: libcosmic hardcodes `radius_s` for the
/// first/last list-item buttons and the list container does not clip its
/// children, so a larger card radius would let pressed/hover backgrounds
/// paint outside the rounded corners.
#[must_use]
pub fn section_card() -> cosmic::theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();

        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(Background::Color(elevated_fill(cosmic))),
            border: Border {
                color: Color::from(cosmic.accent_color().with_alpha(0.12)),
                radius: cosmic.corner_radii.radius_s.into(),
                width: 1.0,
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.043, 0.18, 0.10),
                offset: Vector::new(0.0, 2.0),
                blur_radius: 16.0,
            },
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

// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::{Background, Border, Color, Shadow, Vector};
use cosmic::widget::{button, container};
use cosmic::{Theme, theme};

const CHROME_ALPHA: f32 = 0.62;
const CONTENT_ALPHA: f32 = 0.92;
const DASHBOARD_ALPHA: f32 = 0.72;
const MENU_ALPHA: f32 = 0.68;
const PATH_ALPHA: f32 = 0.58;

fn material(color: impl Into<Color>, alpha: f32, frosted: bool) -> Color {
    let mut color = color.into();
    if frosted {
        color.a = alpha;
    }
    color
}

fn accent(theme: &Theme, alpha: f32) -> Color {
    let mut color: Color = theme.cosmic().accent_color().into();
    color.a = alpha;
    color
}

/// Neutral hairline for glass chrome.
///
/// `on_bg_color` is near-black on a light theme and near-white on a dark one,
/// so one low-alpha expression edges a surface correctly in both without
/// tinting the glass it borders.
fn hairline(theme: &Theme, alpha: f32) -> Color {
    let mut color: Color = theme.cosmic().on_bg_color().into();
    color.a = alpha;
    color
}

fn depth_shadow(theme: &Theme, alpha: f32, y: f32, blur_radius: f32) -> Shadow {
    let alpha = if theme.cosmic().is_dark {
        alpha
    } else {
        alpha * 0.45
    };
    Shadow {
        color: Color::from_rgba(0.0, 0.0, 0.0, alpha),
        offset: Vector::new(0.0, y),
        blur_radius,
    }
}

pub fn toolbar(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.on_bg_component_color().into()),
        text_color: Some(cosmic.on_bg_component_color().into()),
        background: Some(Background::Color(material(
            cosmic.bg_component_color(),
            CHROME_ALPHA,
            cosmic.is_frosted,
        ))),
        shadow: depth_shadow(theme, 0.10, 4.0, 16.0),
        snap: true,
        ..Default::default()
    }
}

pub fn tab_strip(theme: &Theme) -> container::Style {
    let mut style = toolbar(theme);
    style.shadow = Shadow::default();
    style
}

pub fn content(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.on_bg_color().into()),
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(material(
            cosmic.bg_color(),
            CONTENT_ALPHA,
            cosmic.is_frosted,
        ))),
        border: Border {
            color: hairline(theme, 0.07),
            width: 1.0,
            radius: cosmic.radius_s().into(),
        },
        snap: true,
        ..Default::default()
    }
}

/// Backdrop for the desktop icon grid.
///
/// In `Mode::Desktop` the tab is stretched across the entire output behind the
/// icon grid, so giving it [`content`]'s surface paints a full-screen sheet
/// over the wallpaper — at `CONTENT_ALPHA` that reads as a large rounded
/// rectangle covering the desktop, hairline and all. Out here the wallpaper is
/// the surface. Keep the foreground colours so icon labels stay legible and
/// drop the fill, border and radius.
pub fn desktop_content(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.on_bg_color().into()),
        text_color: Some(cosmic.on_bg_color().into()),
        background: None,
        ..Default::default()
    }
}

pub fn navigation(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.on_bg_color().into()),
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(material(
            cosmic.primary.base,
            CHROME_ALPHA,
            cosmic.is_frosted,
        ))),
        border: Border {
            color: hairline(theme, 0.11),
            width: 1.0,
            radius: cosmic.radius_s().into(),
        },
        shadow: depth_shadow(theme, 0.12, 3.0, 18.0),
        snap: true,
    }
}

pub fn path_bar(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.on_bg_component_color().into()),
        text_color: Some(cosmic.on_bg_component_color().into()),
        background: Some(Background::Color(material(
            cosmic.bg_component_color(),
            PATH_ALPHA,
            cosmic.is_frosted,
        ))),
        border: Border {
            color: hairline(theme, 0.12),
            width: 1.0,
            radius: cosmic.radius_l().into(),
        },
        shadow: depth_shadow(theme, 0.10, 3.0, 14.0),
        snap: true,
    }
}

pub fn menu(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    let component = &cosmic.background.component;
    container::Style {
        icon_color: Some(component.on.into()),
        text_color: Some(component.on.into()),
        background: Some(Background::Color(material(
            component.base,
            MENU_ALPHA,
            cosmic.is_frosted,
        ))),
        border: Border {
            color: hairline(theme, 0.12),
            width: 1.0,
            radius: cosmic.radius_m().into(),
        },
        shadow: depth_shadow(theme, 0.18, 8.0, 24.0),
        snap: true,
    }
}

pub fn dashboard_card(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.on_bg_color().into()),
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(material(
            cosmic.bg_component_color(),
            DASHBOARD_ALPHA,
            cosmic.is_frosted,
        ))),
        border: Border {
            color: hairline(theme, 0.09),
            width: 1.0,
            radius: cosmic.radius_l().into(),
        },
        shadow: depth_shadow(theme, 0.10, 4.0, 18.0),
        snap: true,
    }
}

pub fn dashboard_accent_card(theme: &Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        icon_color: Some(cosmic.accent_color().into()),
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(accent(
            theme,
            if cosmic.is_frosted { 0.10 } else { 0.07 },
        ))),
        border: Border {
            color: accent(theme, 0.22),
            width: 1.0,
            radius: cosmic.radius_l().into(),
        },
        shadow: depth_shadow(theme, 0.08, 3.0, 16.0),
        snap: true,
    }
}

pub fn dashboard_button() -> theme::Button {
    theme::Button::Custom {
        active: Box::new(|focused, theme| dashboard_button_style(theme, focused, 0.44, 0.12, 0.06)),
        hovered: Box::new(|focused, theme| {
            dashboard_button_style(theme, focused, 0.62, 0.24, 0.12)
        }),
        pressed: Box::new(|focused, theme| {
            dashboard_button_style(theme, focused, 0.72, 0.30, 0.06)
        }),
        disabled: Box::new(|theme| dashboard_button_style(theme, false, 0.30, 0.08, 0.0)),
    }
}

fn dashboard_button_style(
    theme: &Theme,
    focused: bool,
    background_alpha: f32,
    border_alpha: f32,
    shadow_alpha: f32,
) -> button::Style {
    let cosmic = theme.cosmic();
    let mut style = button::Style::new();
    style.background = Some(Background::Color(material(
        cosmic.bg_component_color(),
        background_alpha,
        cosmic.is_frosted,
    )));
    style.icon_color = Some(cosmic.accent_color().into());
    style.text_color = Some(cosmic.on_bg_color().into());
    style.border_radius = cosmic.radius_l().into();
    style.border_width = 1.0;
    style.border_color = hairline(theme, border_alpha);
    style.shadow_offset = Vector::new(0.0, if shadow_alpha > 0.0 { 2.0 } else { 0.0 });
    if focused {
        style.outline_width = 1.0;
        style.outline_color = cosmic.accent_color().into();
    }
    style
}

pub fn ask_claw_button() -> theme::Button {
    theme::Button::Custom {
        active: Box::new(|focused, theme| ask_claw_style(theme, focused, 0.10, 0.22)),
        hovered: Box::new(|focused, theme| ask_claw_style(theme, focused, 0.18, 0.34)),
        pressed: Box::new(|focused, theme| ask_claw_style(theme, focused, 0.24, 0.44)),
        disabled: Box::new(|theme| ask_claw_style(theme, false, 0.06, 0.10)),
    }
}

fn ask_claw_style(
    theme: &Theme,
    focused: bool,
    background_alpha: f32,
    border_alpha: f32,
) -> button::Style {
    let cosmic = theme.cosmic();
    let mut style = button::Style::new();
    style.background = Some(Background::Color(accent(theme, background_alpha)));
    style.icon_color = Some(cosmic.accent_color().into());
    style.text_color = Some(cosmic.accent_text_color().into());
    style.border_radius = cosmic.radius_m().into();
    style.border_width = 1.0;
    style.border_color = accent(theme, border_alpha);
    style.shadow_offset = Vector::new(0.0, if background_alpha >= 0.18 { 1.0 } else { 0.0 });
    if focused {
        style.outline_width = 1.0;
        style.outline_color = cosmic.accent_color().into();
    }
    style
}

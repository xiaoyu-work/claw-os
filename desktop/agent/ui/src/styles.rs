use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::{Background, Border, Color, Shadow};
use cosmic::theme;

pub(crate) fn page(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        text_color: Some(cosmic.background.on.into()),
        background: Some(Background::Color(Color::from(cosmic.background.base))),
        border: Border::default(),
        shadow: Shadow::default(),
        icon_color: Some(cosmic.background.on.into()),
        snap: true,
    }
}

pub(crate) fn sidebar(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.55;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border::default(),
        shadow: Shadow::default(),
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

pub(crate) fn input_card(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.60;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_l().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.10).into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.10),
            offset: cosmic::iced::Vector::new(0.0, 2.0),
            blur_radius: 16.0,
        },
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

pub(crate) fn user_pill(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        text_color: Some(cosmic.accent.on.into()),
        background: Some(Background::Color(Color::from(cosmic.accent.base))),
        border: Border {
            radius: cosmic.corner_radii.radius_l.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.accent.on.into()),
        snap: true,
    }
}

pub(crate) fn active_pill(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    tool_card(theme)
}

pub(crate) fn tool_card(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.55;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_m().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.10).into(),
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

pub(crate) fn tool_error_card(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut style = tool_card(theme);
    style.border.color = cosmic.destructive.base.into();
    style
}

pub(crate) fn orb(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.accent_color();
    fill.alpha = 0.16;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: 75.0.into(),
            width: 4.0,
            color: cosmic.accent_color().with_alpha(0.85).into(),
        },
        shadow: Shadow {
            color: cosmic.accent_color().with_alpha(0.25).into(),
            offset: cosmic::iced::Vector::new(0.0, 0.0),
            blur_radius: 24.0,
        },
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

pub(crate) fn level_bar(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        background: Some(Background::Color(cosmic.accent_color().into())),
        border: Border {
            radius: 3.0.into(),
            ..Border::default()
        },
        ..Default::default()
    }
}

pub(crate) fn green_dot(_: &cosmic::Theme) -> cosmic::widget::container::Style {
    cosmic::widget::container::Style {
        background: Some(Background::Color(Color::from_rgb(0.22, 0.78, 0.36))),
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..Default::default()
    }
}

pub(crate) fn idle_dot(_: &cosmic::Theme) -> cosmic::widget::container::Style {
    cosmic::widget::container::Style::default()
}

pub(crate) fn selected_session() -> cosmic::widget::button::Style {
    let cosmic = theme::active().cosmic().clone();
    cosmic::widget::button::Style {
        background: Some(Background::Color(Color::from(cosmic.accent.base))),
        border_radius: cosmic.corner_radii.radius_s.into(),
        border_color: Color::TRANSPARENT,
        border_width: 0.0,
        outline_color: Color::TRANSPARENT,
        outline_width: 0.0,
        icon_color: Some(cosmic.accent.on.into()),
        text_color: Some(cosmic.accent.on.into()),
        overlay: None,
        shadow_offset: cosmic::iced::Vector::new(0.0, 0.0),
    }
}

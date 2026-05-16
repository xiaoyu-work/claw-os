use crate::{
    fl,
    utils::iced::{IcedElement, Program},
};

use calloop::LoopHandle;
use cosmic::{
    Apply,
    iced::widget::{container, row, space},
    iced_core::{Alignment, Background, Border, Color, Length},
    theme,
    widget::{icon::from_name, text},
};
use smithay::utils::Size;

pub type SwapIndicator = IcedElement<SwapIndicatorInternal>;

pub fn swap_indicator(
    evlh: LoopHandle<'static, crate::state::State>,
    theme: cosmic::Theme,
) -> SwapIndicator {
    SwapIndicator::new(SwapIndicatorInternal, Size::from((1, 1)), evlh, theme)
}

pub struct SwapIndicatorInternal;

impl Program for SwapIndicatorInternal {
    type Message = ();

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        row(vec![
            from_name("window-swap-symbolic")
                .size(32)
                .prefer_svg(true)
                .icon()
                .into(),
            space::horizontal().width(16).into(),
            text::title3(fl!("swap-windows")).into(),
        ])
        .align_y(Alignment::Center)
        .apply(container)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .padding(16)
        .apply(container)
        .class(theme::Container::custom(|theme| container::Style {
            snap: true,
            // ClawOS: accent removed. Use the quieter primary container palette
            // instead of the cosmic accent for swap/resize/hover indicators.
            icon_color: Some(Color::from(theme.cosmic().primary.on)),
            text_color: Some(Color::from(theme.cosmic().primary.on)),
            background: Some(Background::Color(theme.cosmic().primary.base.into())),
            border: Border {
                radius: 18.0.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Default::default(),
        }))
        .width(Length::Shrink)
        .height(Length::Shrink)
        .apply(container)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }
}

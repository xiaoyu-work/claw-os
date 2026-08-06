// popup for rendering overflow items in their own space

use calloop::LoopHandle;
use cosmic::iced::core::Shadow;
use cosmic::iced::{Color, Length, id};
use cosmic::widget::{container, space};
use cosmic::{Theme, theme};

use crate::iced::{Element, IcedElement, Program};
use crate::xdg_shell_wrapper::shared_state::GlobalState;

pub const BORDER_WIDTH: u32 = 1;

pub type OverflowPopupElement = IcedElement<OverflowPopup>;

pub fn overflow_popup_element(
    id: id::Id,
    logical_width: f32,
    logical_height: f32,
    loop_handle: LoopHandle<'static, GlobalState>,
    theme: Theme,
    panel_id: usize,
    count: usize,
) -> OverflowPopupElement {
    IcedElement::new(
        OverflowPopup { id, logical_width, logical_height, count },
        ((logical_width).round() as i32, (logical_height).round() as i32),
        loop_handle,
        theme,
        panel_id,
        false,
    )
}

pub struct OverflowPopup {
    pub id: id::Id,
    pub logical_width: f32,
    pub logical_height: f32,
    pub count: usize,
}

impl Program for OverflowPopup {
    type Message = ();

    fn view(&self) -> Element<'_, ()> {
        let width = self.logical_width;
        let height = self.logical_height;
        let border_width = BORDER_WIDTH as f32;
        Element::from(
            cosmic::widget::container(space::horizontal().width(Length::Fixed(width)))
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .class(theme::Container::custom(move |theme| {
                    let cosmic = theme.cosmic();
                    let radius_m = cosmic.corner_radii.radius_m;

                    // Claw Glass: translucent surface with a brand-blue
                    // hairline and soft elevation, matching applet popups.
                    // The compositor blurs behind panel surfaces, so an
                    // opaque `background.base` fill here read as flat grey.
                    let mut background = Color::from(cosmic.background.base);
                    if cosmic.is_frosted {
                        background.a = 0.82;
                    }
                    // `background.on` is near-black on light and near-white on
                    // dark, so a low alpha of it edges the popup without
                    // tinting it.
                    let mut hairline = Color::from(cosmic.background.on);
                    hairline.a = if cosmic.is_dark { 0.16 } else { 0.10 };
                    let shadow_alpha = if cosmic.is_dark { 0.28 } else { 0.14 };

                    container::Style {
                        text_color: Some(cosmic.background.on.into()),
                        background: Some(background.into()),
                        border: cosmic::iced::Border {
                            radius: radius_m.into(),
                            width: border_width,
                            color: hairline,
                        },
                        shadow: Shadow {
                            color: Color::from_rgba(0.0, 0.0, 0.0, shadow_alpha),
                            offset: cosmic::iced::Vector::new(0.0, 6.0),
                            blur_radius: 24.0,
                        },
                        icon_color: Some(cosmic.background.on.into()),
                        snap: true,
                    }
                })),
        )
    }
}

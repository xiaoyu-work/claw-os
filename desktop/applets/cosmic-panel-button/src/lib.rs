// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use config::{CosmicPanelButtonConfig, IndividualConfig, Override};
use cosmic::desktop::fde::{self, DesktopEntry, get_languages_from_env};
use cosmic::widget::space;
use cosmic::{
    Task, app,
    applet::{
        Size,
        cosmic_panel_config::{PanelAnchor, PanelSize},
    },
    iced::widget::row,
    iced::{self, Length},
    surface,
    widget::{Id, autosize},
};
use cosmic_config::{Config, CosmicConfigEntry};
use std::{env, fs, process::Command, sync::LazyLock};

mod config;

static AUTOSIZE_MAIN_ID: LazyLock<Id> = LazyLock::new(|| Id::new("autosize-main"));

#[derive(Debug, Clone, Default)]
struct Desktop {
    name: String,
    icon: Option<String>,
    /// Icon to use while the shell is on a dark theme, from
    /// `X-ClawIconDark`.
    ///
    /// libcosmic flat-tints anything whose name ends in `-symbolic`, which
    /// is enough for a single-colour glyph but destroys an icon that has to
    /// keep an accent — the claw mark loses its blue dot. Such an icon ships
    /// as an untinted pair instead, and this is the dark half.
    icon_dark: Option<String>,
    exec: String,
    /// Presentation requested by the entry itself via
    /// `X-CosmicAppletPresentation`. The per-panel user config still
    /// wins; this only supplies a per-button default, which the
    /// panel-wide config cannot express.
    presentation: Option<Override>,
}

struct Button {
    core: cosmic::app::Core,
    desktop: Desktop,
    config: IndividualConfig,
}

fn presentation_for(
    anchor: &PanelAnchor,
    size: &Size,
    has_icon: bool,
    forced: Option<Override>,
    requested: Option<Override>,
) -> Override {
    if matches!(requested, Some(Override::Divider)) {
        Override::Divider
    } else if has_icon && matches!(anchor, PanelAnchor::Left | PanelAnchor::Right) {
        Override::Icon
    } else if let Some(forced) = forced {
        forced
    } else if let Some(requested) = requested {
        requested
    } else if matches!(size, Size::PanelSize(PanelSize::XS)) {
        Override::Text
    } else {
        Override::Icon
    }
}

#[derive(Debug, Clone)]
enum Msg {
    Press,
    ConfigUpdated(CosmicPanelButtonConfig),
    Surface(surface::Action),
}

impl Button {
    pub fn icon_button_from_handle<'a, Message: Clone + 'static>(
        &self,
        icon: cosmic::widget::icon::Handle,
    ) -> cosmic::widget::Button<'a, Message> {
        let suggested = self.core.applet.suggested_size(icon.symbolic);
        let (major_padding, applet_padding_minor_axis) =
            self.core.applet.suggested_padding(icon.symbolic);
        let (horizontal_padding, vertical_padding) = if self.core.applet.is_horizontal() {
            (major_padding, applet_padding_minor_axis)
        } else {
            (applet_padding_minor_axis, major_padding)
        };
        let symbolic = icon.symbolic;

        cosmic::widget::button::custom(
            cosmic::widget::layer_container(
                cosmic::widget::icon(icon)
                    .class(if symbolic {
                        cosmic::theme::Svg::Custom(std::rc::Rc::new(|theme| {
                            cosmic::iced::widget::svg::Style {
                                color: Some(theme.cosmic().background.on.into()),
                            }
                        }))
                    } else {
                        cosmic::theme::Svg::default()
                    })
                    .width(Length::Fixed(suggested.0 as f32))
                    .height(Length::Fixed(suggested.1 as f32)),
            )
            .center(Length::Fill),
        )
        .width(Length::Fixed((suggested.0 + 2 * horizontal_padding) as f32))
        .height(Length::Fixed((suggested.1 + 2 * vertical_padding) as f32))
        .class(cosmic::theme::Button::AppletIcon)
    }
}

impl cosmic::Application for Button {
    type Message = Msg;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = Desktop;
    const APP_ID: &'static str = "com.clawos.PanelButton";

    fn init(core: cosmic::app::Core, desktop: Desktop) -> (Self, app::Task<Msg>) {
        let config = Config::new(Self::APP_ID, CosmicPanelButtonConfig::VERSION)
            .ok()
            .and_then(|c| CosmicPanelButtonConfig::get_entry(&c).ok())
            .unwrap_or_default()
            .configs
            .get(&core.applet.panel_type.to_string())
            .cloned()
            .unwrap_or_default();
        (
            Self {
                core,
                desktop,
                config,
            },
            Task::none(),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn update(&mut self, message: Msg) -> app::Task<Msg> {
        match message {
            Msg::Press => {
                let _ = Command::new("sh")
                    .arg("-c")
                    .arg(&self.desktop.exec)
                    .spawn()
                    .unwrap();
            }
            Msg::ConfigUpdated(conf) => {
                self.config = conf
                    .configs
                    .get(&self.core.applet.panel_type.to_string())
                    .cloned()
                    .unwrap_or_default();
            }
            Msg::Surface(a) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(a),
                ));
            }
        }
        Task::none()
    }

    fn view(&self) -> cosmic::Element<'_, Msg> {
        // A divider is always non-interactive. Otherwise panels anchored
        // left or right are too narrow for labels and become icon-only;
        // user overrides then beat the entry's requested presentation.
        let presentation = presentation_for(
            &self.core.applet.anchor,
            &self.core.applet.size,
            self.desktop.icon.is_some(),
            self.config.force_presentation,
            self.desktop.presentation,
        );

        let icon = || {
            let name = match self.desktop.icon_dark {
                Some(ref dark) if cosmic::theme::active().cosmic().is_dark => dark.clone(),
                _ => self.desktop.icon.clone().unwrap_or_default(),
            };
            cosmic::widget::icon::from_name(name).handle()
        };
        // Reserve the panel's full cross-axis extent so a text-bearing
        // button lines up with its icon-only neighbours instead of
        // collapsing to the height of the glyphs.
        let cross_axis_filler = || {
            space::vertical().height(Length::Fixed(
                (self.core.applet.suggested_size(true).1
                    + 2 * self.core.applet.suggested_padding(true).1) as f32,
            ))
        };

        let element: cosmic::Element<'_, Msg> = match presentation {
            Override::Divider => {
                let length = self.core.applet.suggested_size(true).0.saturating_sub(4) as f32;
                if self.core.applet.is_horizontal() {
                    cosmic::widget::container(
                        cosmic::iced::widget::rule::vertical(1).height(Length::Fixed(length)),
                    )
                    .padding([0, 8])
                    .into()
                } else {
                    cosmic::widget::container(
                        cosmic::widget::divider::horizontal::default().width(Length::Fixed(length)),
                    )
                    .padding([8, 0])
                    .into()
                }
            }
            Override::Icon if self.desktop.icon.is_some() => cosmic::Element::from(
                self.core.applet.applet_tooltip::<Msg>(
                    self.icon_button_from_handle(icon())
                        .on_press_down(Msg::Press),
                    self.desktop.name.clone(),
                    false,
                    Msg::Surface,
                    None,
                ),
            ),
            Override::IconAndText if self.desktop.icon.is_some() => {
                let spacing = cosmic::theme::active().cosmic().spacing.space_xxs;
                let icon_size = self.core.applet.suggested_size(true).0;

                let content = row!(
                    cosmic::widget::icon(icon())
                        .width(Length::Fixed(icon_size as f32))
                        .height(Length::Fixed(icon_size as f32)),
                    self.core.applet.text(&self.desktop.name).size(13.0),
                    cross_axis_filler(),
                )
                .spacing(spacing)
                .align_y(iced::Alignment::Center);

                cosmic::widget::button::custom(content)
                    .padding([0, self.core.applet.suggested_padding(true).0])
                    .class(cosmic::theme::Button::AppletIcon)
                    .on_press_down(Msg::Press)
                    .into()
            }
            // Text, or an icon presentation with no icon to show.
            _ => {
                let content = row!(
                    self.core.applet.text(&self.desktop.name).size(13.0),
                    cross_axis_filler(),
                )
                .align_y(iced::Alignment::Center);

                cosmic::widget::button::custom(content)
                    .padding([0, self.core.applet.suggested_padding(true).0])
                    .class(cosmic::theme::Button::AppletIcon)
                    .on_press_down(Msg::Press)
                    .into()
            }
        };

        autosize::autosize(element, AUTOSIZE_MAIN_ID.clone()).into()
    }

    fn subscription(&self) -> iced::Subscription<Self::Message> {
        self.core.watch_config(Self::APP_ID).map(|u| {
            for why in u.errors {
                tracing::error!(why = why.to_string(), "Error watching config");
            }
            Msg::ConfigUpdated(u.config)
        })
    }
}

pub fn run() -> iced::Result {
    let id = env::args()
        .nth(1)
        .expect("Requires desktop file id as argument.");
    let filename = format!("{id}.desktop");
    let mut desktop = None;
    let locales = get_languages_from_env();

    for mut path in fde::default_paths() {
        path.push(&filename);
        if let Ok(bytes) = fs::read_to_string(&path) {
            if let Ok(entry) = DesktopEntry::from_str(&path, &bytes, Some(&locales)) {
                desktop = Some(Desktop {
                    name: entry.name(&locales).map_or_else(
                        || panic!("Desktop file '{filename}' doesn't have `Name`"),
                        |x| x.to_string(),
                    ),
                    icon: entry.icon().map(|x| x.to_string()),
                    icon_dark: entry
                        .desktop_entry("X-ClawIconDark")
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty()),
                    exec: entry.exec().map_or_else(
                        || panic!("Desktop file '{filename}' doesn't have `Exec`"),
                        |x| x.to_string(),
                    ),
                    presentation: entry
                        .desktop_entry("X-CosmicAppletPresentation")
                        .and_then(|v| match v.trim() {
                            "Icon" => Some(Override::Icon),
                            "Text" => Some(Override::Text),
                            "IconAndText" => Some(Override::IconAndText),
                            "Divider" => Some(Override::Divider),
                            other => {
                                tracing::warn!(
                                    "desktop file '{filename}' has unknown \
                                     X-CosmicAppletPresentation '{other}'"
                                );
                                None
                            }
                        }),
                });
                break;
            }
        }
    }
    let desktop = desktop.unwrap_or_else(|| {
        panic!("Failed to find valid desktop file '{filename}' in search paths")
    });
    cosmic::applet::run::<Button>(desktop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divider_beats_vertical_dock_icon_override() {
        assert_eq!(
            presentation_for(
                &PanelAnchor::Left,
                &Size::PanelSize(PanelSize::M),
                true,
                Some(Override::Icon),
                Some(Override::Divider),
            ),
            Override::Divider,
        );
    }

    #[test]
    fn per_entry_brand_presentation_is_used_on_top_panel() {
        assert_eq!(
            presentation_for(
                &PanelAnchor::Top,
                &Size::PanelSize(PanelSize::XS),
                true,
                None,
                Some(Override::IconAndText),
            ),
            Override::IconAndText,
        );
    }

    #[test]
    fn vertical_panels_remain_icon_only() {
        assert_eq!(
            presentation_for(
                &PanelAnchor::Left,
                &Size::PanelSize(PanelSize::M),
                true,
                None,
                Some(Override::IconAndText),
            ),
            Override::Icon,
        );
    }
}

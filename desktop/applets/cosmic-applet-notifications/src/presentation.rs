// SPDX-License-Identifier: GPL-3.0-only

use claw_notification_presentation::{Card, Icon, Preferences, TextRun};
use cosmic::iced::{
    font::{Style, Weight},
    widget::text::Span,
};
use cosmic::widget::icon::{self, Handle};
use std::time::Instant;

#[derive(Debug, Clone)]
pub(crate) struct PresentedCard {
    pub card: Card,
    pub received: Instant,
    pub icon: Option<Handle>,
    pub group_icon: Option<Handle>,
}

impl PresentedCard {
    pub fn new(card: Card) -> Result<Self, claw_notification_presentation::Error> {
        card.validate()?;
        let icon = card.icon.as_ref().map(decoded_icon);
        let group_icon = card.group_icon.as_ref().map(decoded_icon);
        Ok(Self {
            card,
            received: Instant::now(),
            icon,
            group_icon,
        })
    }
}

fn decoded_icon(value: &Icon) -> Handle {
    match value {
        Icon::Rgba {
            width,
            height,
            pixels,
        } => icon::from_raster_pixels(*width, *height, pixels.clone()),
        Icon::Mask {
            width,
            height,
            alpha,
        } => {
            let pixels = alpha
                .iter()
                .flat_map(|alpha| [255, 255, 255, *alpha])
                .collect::<Vec<_>>();
            icon::from_raster_pixels(*width, *height, pixels).symbolic(true)
        }
    }
}

pub(crate) fn spans(body: &[TextRun]) -> Vec<Span<'_, ()>> {
    let accent: cosmic::iced::Color = cosmic::theme::active().cosmic().accent_color().into();
    body.iter()
        .map(|run| {
            Span::new(run.text.as_str())
                .font(cosmic::font::Font {
                    weight: if run.bold {
                        Weight::Bold
                    } else {
                        Weight::Normal
                    },
                    style: if run.italic {
                        Style::Italic
                    } else {
                        Style::Normal
                    },
                    ..cosmic::font::default()
                })
                .underline(run.underline)
                .color_maybe(run.accent.then_some(accent))
        })
        .collect()
}

#[derive(Default)]
pub(crate) struct PreferenceState {
    pub confirmed: Option<Preferences>,
    pub connected: bool,
    pub pending: bool,
    pub error: Option<String>,
}

impl PreferenceState {
    pub fn muted(&self) -> bool {
        self.confirmed.as_ref().is_some_and(|value| value.muted)
    }

    pub fn enabled(&self) -> bool {
        self.connected && self.confirmed.is_some() && !self.pending
    }

    pub fn begin(&mut self, muted: bool) -> bool {
        if !self.enabled() || self.muted() == muted {
            return false;
        }
        self.pending = true;
        self.error = None;
        true
    }

    pub fn observe(&mut self, value: Preferences) {
        self.confirmed = Some(value);
    }

    pub fn finish(&mut self, result: Result<Preferences, String>) {
        self.pending = false;
        match result {
            Ok(value) => {
                self.observe(value);
                self.error = None;
            }
            Err(error) => self.fail(error),
        }
    }

    pub fn disconnect(&mut self, error: String) {
        self.connected = false;
        self.pending = false;
        self.fail(error);
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error.chars().take(4096).collect());
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/presentation.rs"
    ));
}

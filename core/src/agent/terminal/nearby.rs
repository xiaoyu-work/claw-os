//! A sharing center over the same broker; closing it does not stop the radio.

use super::{state::App, InputAction};
use clawd_client::nearby::*;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

pub(super) fn key(app: &mut App, key: KeyEvent) -> Option<InputAction> {
    if !app.nearby_open {
        return None;
    }
    if app.current_approval().is_some()
        || app.approval_detail.is_some()
        || app.confirmation.is_some()
    {
        return None;
    }
    if key.kind != KeyEventKind::Press || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
    {
        return Some(InputAction::None);
    }
    let view = app.nearby_view.clone()?;
    if app.nearby_error.is_some()
        && !matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('r') | KeyCode::Char('x')
        )
    {
        return Some(InputAction::None);
    }
    let action = match key.code {
        KeyCode::Esc => {
            app.nearby_open = false;
            InputAction::None
        }
        KeyCode::Up | KeyCode::PageUp => {
            app.nearby_scroll = app.nearby_scroll.saturating_sub(4);
            InputAction::None
        }
        KeyCode::Down | KeyCode::PageDown => {
            app.nearby_scroll = app.nearby_scroll.saturating_add(4);
            InputAction::None
        }
        KeyCode::Char('r') => InputAction::Nearby(None),
        KeyCode::Char('d') => view.session.as_ref().map_or(InputAction::None, |s| {
            InputAction::Nearby(Some(NearbyAction::Discover {
                session_id: s.id.clone(),
            }))
        }),
        KeyCode::Char('x') => view.session.as_ref().map_or(InputAction::None, |s| {
            InputAction::Nearby(Some(NearbyAction::Stop {
                session_id: s.id.clone(),
            }))
        }),
        KeyCode::Char('a') | KeyCode::Char('n') => view
            .session
            .as_ref()
            .and_then(|s| {
                s.offers
                    .iter()
                    .find(|offer| {
                        Some(&offer.id) == app.nearby_offer.as_ref()
                            && offer.phase == TransferPhase::Offered
                    })
                    .map(|offer| {
                        InputAction::Nearby(Some(NearbyAction::Decide {
                            session_id: s.id.clone(),
                            offer_id: offer.id.clone(),
                            accept: key.code == KeyCode::Char('a'),
                        }))
                    })
            })
            .unwrap_or(InputAction::None),
        KeyCode::Left | KeyCode::Right => {
            if let Some(session) = &view.session {
                if !session.offers.is_empty() {
                    let current = app
                        .nearby_offer
                        .as_ref()
                        .and_then(|id| session.offers.iter().position(|offer| &offer.id == id));
                    let next = match (current, key.code) {
                        (Some(index), KeyCode::Left) => {
                            (index + session.offers.len() - 1) % session.offers.len()
                        }
                        (Some(index), _) => (index + 1) % session.offers.len(),
                        (None, _) => 0,
                    };
                    app.nearby_offer = Some(session.offers[next].id.clone());
                    app.nearby_scroll = 0;
                }
            }
            InputAction::None
        }
        KeyCode::Char('o') | KeyCode::Char('i') => {
            app.nearby_open = false;
            app.prefill_input(if key.code == KeyCode::Char('i') {
                "/share receive --seconds 300".into()
            } else {
                "/share open --seconds 300".into()
            });
            InputAction::None
        }
        KeyCode::Char('s') => {
            app.nearby_open = false;
            app.prefill_input("/share send PEER_ID | [\"/home/user/file.pdf\"]".into());
            InputAction::None
        }
        _ => InputAction::None,
    };
    Some(action)
}

pub(super) fn render(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    if !app.nearby_open || app.current_approval().is_some() || app.approval_detail.is_some() {
        return;
    }
    let Some(view) = &app.nearby_view else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(110);
    let height = screen.height.saturating_sub(4);
    if width < 40 || height < 8 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let mut lines=vec![Line::raw("Left/Right selects one exact offer; A accepts that offer, N declines it. Device names are unverified.")];
    if let Some(offer) = view.session.as_ref().and_then(|s| {
        s.offers
            .iter()
            .find(|offer| Some(&offer.id) == app.nearby_offer.as_ref())
    }) {
        lines.push(Line::raw(format!(
            "Selected: {} · {} · {:?} · expires {}",
            offer.sender, offer.id, offer.phase, offer.expires_at
        )));
        for file in &offer.files {
            lines.push(Line::raw(format!(
                "  {} · {:?} bytes",
                file.name, file.bytes
            )));
        }
    } else {
        lines.push(Line::raw(
            "No offer selected. Receiving never auto-selects or auto-accepts a different offer.",
        ));
    }
    if let Some(error) = &app.nearby_error {
        lines.push(Line::raw(format!(
            "Sharing view is stale: {error}. Refresh before changing it."
        )));
    }
    lines.extend(
        crate::nearby::presentation(view)
            .lines()
            .map(|line| Line::raw(line.to_string())),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.nearby_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Nearby Sharing - R refresh, Esc detach "),
            ),
        area,
    );
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/terminal/nearby.rs"
    ));
}

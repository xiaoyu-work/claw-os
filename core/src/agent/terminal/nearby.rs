//! A sharing center over the same broker; closing it does not stop the radio.

use super::{backend::Backend, requests::Request};
use super::{state::App, InputAction};
use clawd_client::nearby::*;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use std::sync::Arc;

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
            app.nearby_generation = app.nearby_generation.wrapping_add(1);
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

pub(super) fn request(
    app: &mut App,
    backend: Arc<dyn Backend>,
    action: Option<NearbyAction>,
) -> Request {
    app.nearby_generation = app.nearby_generation.wrapping_add(1);
    let generation = app.nearby_generation;
    let mutation = action.is_some();
    let request = Request::new(
        "Updating Nearby Sharing",
        async move { Ok(backend.nearby(action).await) },
        move |app, result| apply_view(app, generation, result),
    );
    if mutation {
        request.mutation()
    } else {
        request
    }
}

pub(super) fn command(app: &mut App, backend: Arc<dyn Backend>, rest: String) -> Request {
    app.nearby_generation = app.nearby_generation.wrapping_add(1);
    let generation = app.nearby_generation;
    let view = app.nearby_view.clone();
    let mutation = !matches!(rest.trim(), "" | "status");
    let request = Request::new(
        "Loading Nearby Sharing",
        async move {
            let result = async {
                let mut parts = rest.splitn(2, char::is_whitespace);
                let verb = parts.next().unwrap_or("").trim();
                let tail = parts.next().unwrap_or("").trim();
                let action = if matches!(verb, "" | "status") {
                    None
                } else {
                    let view = match view {
                        Some(view) => view,
                        None => backend.nearby(None).await?,
                    };
                    let args = if verb == "send" {
                        let (peer, files) = tail
                            .split_once('|')
                            .ok_or("Use /share send PEER_ID | [\"/absolute/file\"]")?;
                        let files: Vec<String> =
                            serde_json::from_str(files).map_err(|error| error.to_string())?;
                        let mut args = vec![peer.trim().to_string()];
                        args.extend(files);
                        args
                    } else {
                        tail.split_whitespace().map(str::to_string).collect()
                    };
                    Some(crate::nearby::parse(verb, &args, &view)?)
                };
                backend.nearby(action).await
            }
            .await;
            Ok(result)
        },
        move |app, result| {
            apply_view(app, generation, result)?;
            if app.nearby_generation == generation {
                app.nearby_scroll = 0;
            }
            Ok(())
        },
    );
    if mutation {
        request.mutation()
    } else {
        request
    }
}

fn apply_view(
    app: &mut App,
    generation: u64,
    result: Result<NearbyView, String>,
) -> Result<(), String> {
    match result {
        Ok(view) if generation == app.nearby_generation => {
            app.nearby_view = Some(view);
            app.nearby_open = true;
            app.nearby_error = None;
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) => {
            if generation == app.nearby_generation {
                app.nearby_error = Some(error.clone());
            }
            Err(error)
        }
    }
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

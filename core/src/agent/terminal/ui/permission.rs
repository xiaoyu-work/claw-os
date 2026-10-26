use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::backend::ReviewDecision;
use super::super::state::{clean_text, App};

fn body(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(request) = app.current_approval() else {
        return Vec::new();
    };
    let fields = [
        (
            format!(
                "{}  [{}]",
                clean_text(&request.label),
                clean_text(request.risk.as_deref().unwrap_or("unclassified")).to_uppercase()
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        (
            format!("Target: {}", clean_text(&request.target)),
            Style::default(),
        ),
        (
            clean_text(&request.description),
            Style::default().fg(Color::Gray),
        ),
    ];
    fields
        .into_iter()
        .flat_map(|(text, style)| {
            wrap(&text, width)
                .into_iter()
                .map(move |line| Line::styled(line, style))
        })
        .collect()
}

fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for original in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for word in original.split_inclusive(char::is_whitespace) {
            let word_width = UnicodeWidthStr::width(word);
            if used > 0 && word_width <= width && used + word_width > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            for character in word.chars() {
                let columns = character.width().unwrap_or(0);
                if used > 0 && used + columns > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(character);
                used += columns;
            }
        }
        lines.push(line);
    }
    lines
}

fn action_rows(width: u16) -> u16 {
    if width >= 28 {
        1
    } else {
        2
    }
}

pub(super) fn height(width: u16, app: &App) -> u16 {
    let inner_width = width.saturating_sub(2);
    let content = body(app, inner_width).len().min(usize::from(u16::MAX)) as u16;
    content
        .saturating_add(action_rows(inner_width))
        .saturating_add(2)
}

pub(super) fn scroll_limit(area: Rect, app: &App) -> u16 {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let visible = inner.height.saturating_sub(action_rows(inner.width));
    body(app, inner.width)
        .len()
        .saturating_sub(usize::from(visible))
        .min(usize::from(u16::MAX)) as u16
}

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, app: &App, block: Block<'_>) {
    let inner = block.inner(area);
    let lines = body(app, inner.width);
    let controls = action_rows(inner.width).min(inner.height);
    let content_height = inner.height.saturating_sub(controls);
    let max_scroll = lines.len().saturating_sub(usize::from(content_height));
    let offset = usize::from(app.approval_scroll).min(max_scroll);
    let title = if max_scroll > 0 {
        format!(
            " Permission {}-{}/{} PgUp/PgDn ",
            offset + 1,
            (offset + usize::from(content_height)).min(lines.len()),
            lines.len(),
        )
    } else {
        " Permission ".to_string()
    };
    frame.render_widget(block.title(title), area);
    let content = Rect::new(inner.x, inner.y, inner.width, content_height);
    frame.render_widget(
        Paragraph::new(lines).scroll((offset.min(usize::from(u16::MAX)) as u16, 0)),
        content,
    );
    let style = |decision, color| {
        if app.approval_choice == decision {
            Style::default()
                .fg(Color::White)
                .bg(color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };
    let approve = Span::styled(
        "[a] Authorize once",
        style(ReviewDecision::ApproveOnce, Color::Magenta),
    );
    let deny = Span::styled("[d] Deny", style(ReviewDecision::Deny, Color::Red));
    let actions = if controls == 1 {
        vec![Line::from(vec![approve, Span::raw("  "), deny])]
    } else {
        vec![Line::from(approve), Line::from(deny)]
    };
    frame.render_widget(
        Paragraph::new(actions),
        Rect::new(inner.x, inner.y + content_height, inner.width, controls),
    );
}

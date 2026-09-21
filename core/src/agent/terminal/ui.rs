use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::commands::suggestions;
use super::state::{App, ApprovalStatus, Entry, EntryKind, RunStatus, ToolStatus};

pub(super) fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    render_header(frame, chunks[0], app);
    render_transcript(frame, chunks[1], app);
    render_composer(frame, chunks[2], app);
    render_footer(frame, chunks[3], app);
    render_command_palette(frame, chunks[1], app);
}

fn render_command_palette(frame: &mut Frame<'_>, transcript: Rect, app: &App) {
    let suggestions = suggestions(&app.input);
    if suggestions.is_empty() || app.current_approval().is_some() {
        return;
    }
    let width = transcript.width.saturating_sub(4).min(64);
    let height = (suggestions.len() as u16 + 2).min(transcript.height);
    if width < 20 || height < 3 {
        return;
    }
    let area = Rect::new(
        transcript.x + 2,
        transcript.bottom().saturating_sub(height),
        width,
        height,
    );
    let lines = suggestions
        .into_iter()
        .map(|(command, description)| {
            Line::from(vec![
                Span::styled(format!("{command:<12}"), Style::default().fg(Color::Cyan)),
                Span::styled(description, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Commands - Tab completes "),
        ),
        area,
    );
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let status = match app.status {
        RunStatus::Ready => Span::styled("READY", Style::default().fg(Color::Green)),
        RunStatus::Working => Span::styled("WORKING", Style::default().fg(Color::Cyan)),
        RunStatus::WaitingApproval => Span::styled("APPROVAL", Style::default().fg(Color::Magenta)),
        RunStatus::Cancelling => Span::styled("STOPPING", Style::default().fg(Color::Yellow)),
    };
    let short_session = app.conversation.id.chars().take(22).collect::<String>();
    let line = Line::from(vec![
        Span::styled(
            " CLAW AGENT ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        status,
        Span::raw("  "),
        Span::styled(&app.selected_model, Style::default().fg(Color::Blue)),
        Span::raw("  "),
        Span::styled(short_session, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .title(app.conversation.title.as_str()),
        ),
        area,
    );
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let lines = transcript_lines(&app.entries);
    let width = area.width.saturating_sub(2).max(1);
    let content_width = usize::from(width);
    let total = lines
        .iter()
        .map(|line| {
            let line_width = line.width().max(1);
            line_width.div_ceil(content_width)
        })
        .sum::<usize>();
    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(Block::default().padding(Padding::horizontal(1)));
    let visible = usize::from(area.height.saturating_sub(1));
    let maximum = total.saturating_sub(visible);
    let offset = maximum.saturating_sub(usize::from(app.scroll));
    frame.render_widget(
        paragraph.scroll((offset.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn transcript_lines(entries: &[Entry]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for entry in entries {
        match &entry.kind {
            EntryKind::User => {
                for (index, text) in entry.text.lines().enumerate() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if index == 0 { "> " } else { "  " },
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(text.to_string(), Style::default().fg(Color::White)),
                    ]));
                }
            }
            EntryKind::Assistant => {
                lines.extend(markdown_lines(&entry.text));
            }
            EntryKind::Reasoning => {
                for (index, text) in entry.text.lines().enumerate() {
                    lines.push(Line::styled(
                        format!(
                            "{}{}",
                            if index == 0 {
                                "thinking: "
                            } else {
                                "          "
                            },
                            text
                        ),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            EntryKind::Tool { status, .. } => {
                let (mark, color) = match status {
                    ToolStatus::Running => ("*", Color::Cyan),
                    ToolStatus::Succeeded => ("+", Color::Green),
                    ToolStatus::Failed => ("!", Color::Red),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(entry.text.clone(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            EntryKind::System => {
                lines.extend(entry.text.lines().map(|line| {
                    Line::styled(line.to_string(), Style::default().fg(Color::DarkGray))
                }))
            }
            EntryKind::Error => {
                for (index, text) in entry.text.lines().enumerate() {
                    lines.push(Line::styled(
                        format!("{}{}", if index == 0 { "error: " } else { "       " }, text),
                        Style::default().fg(Color::Red),
                    ));
                }
            }
            EntryKind::Approval { status, .. } => {
                let (label, color) = match status {
                    ApprovalStatus::Pending => ("approval required", Color::Magenta),
                    ApprovalStatus::Approved => ("approved once", Color::Green),
                    ApprovalStatus::Denied => ("denied", Color::Red),
                    ApprovalStatus::Closed => ("closed", Color::DarkGray),
                };
                for (index, text) in entry.text.lines().enumerate() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if index == 0 {
                                format!("{label}: ")
                            } else {
                                "  ".to_string()
                            },
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(text.to_string()),
                    ]));
                }
            }
        }
        lines.push(Line::default());
    }
    lines
}

fn markdown_lines(value: &str) -> Vec<Line<'static>> {
    let mut code = false;
    value
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                code = !code;
                return Line::styled(line.to_string(), Style::default().fg(Color::DarkGray));
            }
            if code {
                return Line::styled(format!("  {line}"), Style::default().fg(Color::Yellow));
            }
            if let Some(title) = line.strip_prefix("### ") {
                return Line::styled(
                    title.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
            }
            if let Some(title) = line.strip_prefix("## ").or_else(|| line.strip_prefix("# ")) {
                return Line::styled(
                    title.to_string(),
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                );
            }
            if line.starts_with("- ") || line.starts_with("* ") {
                return Line::from(vec![
                    Span::styled("  - ", Style::default().fg(Color::Cyan)),
                    Span::raw(line[2..].to_string()),
                ]);
            }
            Line::raw(line.to_string())
        })
        .collect()
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.current_approval().is_some() {
            Color::Magenta
        } else {
            Color::Cyan
        }))
        .title(" Message ");
    if let Some(approval) = app.current_approval() {
        let text = Line::from(vec![
            Span::styled(
                format!("{} {} ", approval.verb, approval.scope),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[a] authorize once  [d] deny  [Esc] stop task",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(text).block(block), area);
        return;
    }

    let display = format!("> {}", app.input);
    frame.render_widget(Paragraph::new(display).block(block), area);
    let inner_width = area.width.saturating_sub(4).max(1);
    let before = app.input.chars().take(app.cursor).collect::<String>();
    let width = UnicodeWidthStr::width(before.as_str()) as u16;
    let x = area.x + 3 + (width % inner_width);
    let y = area.y + 1 + (width / inner_width).min(area.height.saturating_sub(2));
    frame.set_cursor_position(Position::new(
        x.min(area.right().saturating_sub(1)),
        y.min(area.bottom().saturating_sub(1)),
    ));
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let queue = if app.queued_prompts.is_empty() {
        String::new()
    } else {
        format!("  queued {}", app.queued_prompts.len())
    };
    let line = Line::from(vec![
        Span::styled(
            " Enter send  Esc stop  PgUp/PgDn scroll  /help ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(
                "tokens {} in / {} out / {} cached{queue}",
                app.usage_input, app.usage_output, app.usage_cached
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::super::state::{App, EntryKind, RunStatus, ToolStatus};
use super::accent;

pub(super) fn spinner(frame: u64) -> &'static str {
    ["|", "/", "-", "\\"][(frame as usize) % 4]
}

fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if app.active_task.is_none() || area.is_empty() {
        return;
    }
    let tools = || {
        app.entries
            .iter()
            .rev()
            .filter_map(|entry| match entry.kind {
                EntryKind::Tool { status, .. } => Some((entry.text.as_str(), status)),
                _ => None,
            })
    };
    let active = tools()
        .find(|(_, status)| matches!(status, ToolStatus::Running { .. }))
        .or_else(|| tools().find(|(_, status)| *status == ToolStatus::Preparing));
    let task_time = app.task_elapsed().map(elapsed).unwrap_or_default();
    let (mark, color, label, detail) = match app.status {
        RunStatus::Working => match active {
            Some((name, ToolStatus::Running { started_at })) => (
                spinner(app.frame),
                accent(app),
                format!("Running {name}"),
                format!(
                    "Tool {} | Esc stop | waiting for result",
                    elapsed(started_at.elapsed())
                ),
            ),
            Some((name, _)) => (
                spinner(app.frame),
                accent(app),
                format!("Preparing {name}"),
                format!("Task {task_time} | Esc stop | execution not started"),
            ),
            None => (
                spinner(app.frame),
                accent(app),
                if app.provider_had_text() {
                    "Receiving response".into()
                } else {
                    "Waiting for agent response".into()
                },
                format!("Task {task_time} | Esc stop"),
            ),
        },
        RunStatus::Reconnecting => (
            spinner(app.frame),
            Color::Yellow,
            match active {
                Some((name, _)) => format!("Reconnecting - last tool: {name}"),
                None => "Reconnecting to task".into(),
            },
            format!("Task {task_time} | Esc stop | execution status unconfirmed"),
        ),
        RunStatus::WaitingApproval => (
            "?",
            Color::Magenta,
            "Waiting for approval".into(),
            format!("Task {task_time} | Left/Right + Enter | Esc stop"),
        ),
        RunStatus::Cancelling => (
            spinner(app.frame),
            Color::Yellow,
            match active {
                Some((name, _)) => format!("Stopping - last tool: {name}"),
                None => "Stopping task".into(),
            },
            format!("Task {task_time} | waiting for stop confirmation"),
        ),
        RunStatus::Ready => return,
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!(" {mark} "), Style::default().fg(color)),
                Span::styled(
                    label,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::styled(format!("   {detail}"), Style::default().fg(Color::DarkGray)),
        ]),
        area,
    );
}

pub(super) fn tool_line(name: &str, status: ToolStatus, app: &App) -> Line<'static> {
    let (mark, color, detail) = match status {
        ToolStatus::Preparing => (".", Color::DarkGray, "preparing".into()),
        ToolStatus::Running { started_at } => match app.status {
            RunStatus::Working => (
                spinner(app.frame),
                Color::Cyan,
                format!("running {}", elapsed(started_at.elapsed())),
            ),
            RunStatus::Cancelling => (
                spinner(app.frame),
                Color::Yellow,
                "waiting for stop confirmation".into(),
            ),
            RunStatus::WaitingApproval => ("?", Color::Magenta, "waiting for approval".into()),
            RunStatus::Ready | RunStatus::Reconnecting => {
                ("?", Color::Yellow, "execution status unconfirmed".into())
            }
        },
        ToolStatus::Succeeded { duration_ms } => (
            "+",
            Color::Green,
            duration_ms.map(|ms| format!("{ms}ms")).unwrap_or_default(),
        ),
        ToolStatus::Failed { duration_ms } => (
            "!",
            Color::Red,
            duration_ms.map(|ms| format!("{ms}ms")).unwrap_or_default(),
        ),
        ToolStatus::Unfinished => ("?", Color::Yellow, "no result reported".into()),
    };
    Line::from(vec![
        Span::styled(format!("{mark} "), Style::default().fg(color)),
        Span::styled(name.to_string(), Style::default().fg(Color::Gray)),
        Span::styled(
            if detail.is_empty() {
                detail
            } else {
                format!("  {detail}")
            },
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{elapsed, spinner};

    #[test]
    fn elapsed_time_advances_and_formats_long_runs() {
        assert_eq!(elapsed(Duration::ZERO), "0s");
        assert_eq!(elapsed(Duration::from_secs(1)), "1s");
        assert_eq!(elapsed(Duration::from_secs(2)), "2s");
        assert_eq!(elapsed(Duration::from_secs(65)), "1m 05s");
    }

    #[test]
    fn spinner_advances_on_each_frame() {
        assert_ne!(spinner(0), spinner(1));
        assert_ne!(spinner(1), spinner(2));
        assert_eq!(spinner(0), spinner(4));
    }
}

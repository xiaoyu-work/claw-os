use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::commands::suggestions;
use super::state::{
    clean_text, App, ApprovalStatus, Entry, EntryKind, PickerKind, RunStatus, ToolStatus,
};

pub(super) fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let composer_height = composer_height(area.width, app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
    render_header(frame, chunks[0], app);
    render_transcript(frame, chunks[1], app);
    render_composer(frame, chunks[2], app);
    render_footer(frame, chunks[3], app);
    render_command_palette(frame, chunks[1], app);
    render_picker(frame, area, app);
    render_task_detail(frame, area, app);
    render_approval_detail(frame, area, app);
    render_notification_detail(frame, area, app);
    render_notification_preferences(frame, area, app);
    render_activity_detail(frame, area, app);
    render_activity_attention(frame, area, app);
    render_activity_controls(frame, area, app);
    render_activity_evidence(frame, area, app);
    render_activity_operation_preview(frame, area, app);
    render_confirmation(frame, area, app);
}

fn render_activity_evidence(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(evidence) = &app.activity_evidence else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(106);
    let height = screen.height.saturating_sub(4).min(34);
    if width < 44 || height < 14 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let mut lines = vec![Line::styled(
        "Reported evidence and inert references - not authority, truth, or confirmed effects",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )];
    lines.extend(
        evidence
            .presentation
            .lines()
            .map(|line| Line::raw(line.to_string())),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.activity_evidence_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue))
                    .title(" Activity evidence ")
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            "[p] Prepare operation preview",
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn render_activity_operation_preview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(preview) = &app.activity_operation_preview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(100);
    let height = screen.height.saturating_sub(4).min(32);
    if width < 44 || height < 14 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let mut lines = vec![Line::styled(
        "Metadata preview only - authorization not checked, App not executed, effects not confirmed",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )];
    lines.extend(
        preview
            .presentation
            .lines()
            .map(|line| Line::raw(line.to_string())),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.activity_operation_preview_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow))
                    .title(" Activity operation preview ")
                    .title_bottom(Line::from(vec![
                        Span::styled("[e] Back to evidence", Style::default().fg(Color::Yellow)),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn render_activity_controls(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(controls) = &app.activity_controls else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(104);
    let height = screen.height.saturating_sub(4).min(34);
    if width < 44 || height < 14 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let mut lines = vec![
        Line::styled(
            "Constraints and accounting only - never authority or completion",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!("Activity: {}", controls.activity_id)),
    ];
    append_activity_control(
        &mut lines,
        "Execution limits",
        controls.execution_limits.as_ref(),
    );
    append_activity_control(
        &mut lines,
        "Monetary budget (configured accounting, not an invoice)",
        controls.monetary_budget.as_ref(),
    );
    append_activity_control(
        &mut lines,
        "Pending admission priority (no preemption)",
        controls.scheduling_policy.as_ref(),
    );
    append_activity_control(
        &mut lines,
        "Capability policy (constraints, never grants)",
        controls.capability_policy.as_ref(),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.activity_controls_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta))
                    .title(" Activity controls ")
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            "[l] Limits  [b] Budget  [p] Priority  [c] Capability  [r] Refresh",
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn append_activity_control(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    policy: Option<&serde_json::Value>,
) {
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let Some(policy) = policy else {
        lines.push(Line::styled(
            "not configured",
            Style::default().fg(Color::DarkGray),
        ));
        return;
    };
    let mut policy = policy.clone();
    if let Some(policy) = policy.as_object_mut() {
        policy.remove("owner_uid");
    }
    let rendered = serde_json::to_string_pretty(&policy)
        .map(|value| clean_text(&value))
        .unwrap_or_else(|_| "[control unavailable]".into());
    lines.extend(rendered.lines().map(|line| Line::raw(line.to_string())));
}

fn render_activity_detail(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(detail) = &app.activity_detail else {
        return;
    };
    let activity = &detail.activity;
    let width = screen.width.saturating_sub(4).min(100);
    let height = screen.height.saturating_sub(4).min(32);
    if width < 40 || height < 14 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let state_color = match activity.state.as_str() {
        "active" => Color::Green,
        "paused" => Color::Yellow,
        "completed" => Color::Blue,
        "cancelled" => Color::DarkGray,
        _ => Color::Red,
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                activity.state.to_uppercase(),
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(activity.id.clone(), Style::default().fg(Color::DarkGray)),
        ]),
        Line::raw(format!(
            "created: {} | updated: {}",
            activity.created_at, activity.updated_at
        )),
        Line::raw(""),
        Line::styled(
            activity.title.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "Goal",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    lines.extend(markdown_lines(&activity.goal));
    if !activity.completion_criteria.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Completion criteria",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_lines(&activity.completion_criteria));
    }
    if !activity.boundaries.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Planning boundaries (not authority)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_lines(&activity.boundaries));
    }
    if !activity.resources.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Inert resources",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(
            activity
                .resources
                .iter()
                .map(|resource| Line::raw(format!("- {}: {}", resource.label, resource.reference))),
        );
    }
    if let Some(note) = &activity.completion_note {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Completion confirmation",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_lines(note));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!("Recent tasks ({})", detail.jobs.len()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    if detail.jobs.is_empty() {
        lines.push(Line::styled(
            "No associated work.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    for job in &detail.jobs {
        lines.push(Line::raw(format!(
            "- {} {} | {}{}",
            job.status.to_uppercase(),
            job.title,
            job.id,
            if job.waiting_on > 0 {
                format!(" | waiting:{}", job.waiting_on)
            } else {
                String::new()
            }
        )));
        if let Some(error) = &job.error {
            lines.push(Line::styled(
                format!("  error: {error}"),
                Style::default().fg(Color::Red),
            ));
        } else if let Some(response) = &job.response {
            lines.push(Line::styled(
                format!("  result: {response}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    if !detail.sessions.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::raw(format!(
            "sessions: {}",
            detail.sessions.join(", ")
        )));
    }
    let actions = match activity.state.as_str() {
        "active" => {
            "[r] Run  [p] Pause  [c] Complete  [x] Cancel  [a] Attention  [o] Controls  [e] Evidence"
        }
        "paused" => {
            "[u] Resume  [c] Complete  [x] Cancel  [a] Attention  [o] Controls  [e] Evidence"
        }
        "completed" | "cancelled" => "[u] Reopen  [a] Attention  [o] Controls  [e] Evidence",
        _ => "[a] Attention  [o] Controls  [e] Evidence",
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.activity_detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(state_color))
                    .title(" Activity ")
                    .title_bottom(Line::from(vec![
                        Span::styled(actions, Style::default().fg(Color::Yellow)),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn render_activity_attention(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(attention) = &app.activity_attention else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(100);
    let height = screen.height.saturating_sub(4).min(32);
    if width < 40 || height < 12 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let lines = attention
        .presentation
        .lines()
        .map(|line| Line::raw(line.to_string()))
        .collect::<Vec<_>>();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.activity_attention_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta))
                    .title(format!(
                        " Activity attention - {} ({}) ",
                        attention.activity_id, attention.activity_state
                    ))
                    .title_bottom(Line::styled(
                        "Up/Down scroll  Esc close",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_notification_detail(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(notification) = &app.notification_detail else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(96);
    let height = screen.height.saturating_sub(4).min(30);
    if width < 36 || height < 12 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let severity_color = match notification.severity.as_str() {
        "critical" => Color::Red,
        "error" => Color::LightRed,
        "warning" => Color::Yellow,
        _ => Color::Cyan,
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                notification.severity.to_uppercase(),
                Style::default()
                    .fg(severity_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                notification.state.to_uppercase(),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::styled(
                notification.id.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::raw(format!(
            "source: {} | kind: {} | policy: {}",
            notification.source, notification.kind, notification.delivery_policy
        )),
        Line::raw(format!(
            "created: {} | updated: {} | occurrences: {}",
            notification.created_at_ms, notification.updated_at_ms, notification.occurrences
        )),
    ];
    for (label, value) in [
        ("task", notification.task_id.as_deref()),
        ("session", notification.session_id.as_deref()),
        ("job", notification.job_id.as_deref()),
    ] {
        if let Some(value) = value {
            lines.push(Line::raw(format!("{label}: {value}")));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        notification.title.clone(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(markdown_lines(&notification.body));
    if !notification.actions.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Actions (display only)",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(
            notification
                .actions
                .iter()
                .map(|action| Line::raw(format!("- {}: {}", action.label, action.uri))),
        );
    }
    if !notification.deliveries.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Delivery",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(notification.deliveries.iter().map(|delivery| {
            Line::raw(format!(
                "- {}: {} (attempts: {}){}",
                delivery.channel,
                delivery.state,
                delivery.attempts,
                delivery
                    .last_error_code
                    .as_deref()
                    .map(|error| format!(" [{error}]"))
                    .unwrap_or_default()
            ))
        }));
    }
    let mut actions = Vec::new();
    if notification.state == "unread" {
        actions.push("[m] Read");
    }
    if matches!(notification.state.as_str(), "unread" | "read") {
        actions.push("[a] Acknowledge");
    }
    if notification.state != "dismissed" {
        actions.push("[d] Dismiss");
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.notification_detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(severity_color))
                    .title(" Notification Inbox ")
                    .title_bottom(Line::from(vec![
                        Span::styled(actions.join("  "), Style::default().fg(Color::Yellow)),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn render_notification_preferences(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(preferences) = &app.notification_preferences else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(88);
    let height = screen.height.saturating_sub(4).min(25);
    if width < 36 || height < 12 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let enabled = |value| if value { "enabled" } else { "disabled" };
    let dnd = match (
        preferences.dnd_start_minute_utc,
        preferences.dnd_end_minute_utc,
    ) {
        (Some(start), Some(end)) => format!(
            "{}-{} UTC",
            format_utc_minute(start),
            format_utc_minute(end)
        ),
        _ => "off".into(),
    };
    let mut lines = vec![
        Line::styled(
            "Delivery channels",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!(
            "web: {} | minimum: {}",
            enabled(preferences.web_enabled),
            preferences.web_min_severity
        )),
        Line::raw(format!(
            "desktop: {} | minimum: {}",
            enabled(preferences.desktop_enabled),
            preferences.desktop_min_severity
        )),
        Line::raw(format!(
            "ntfy: {} | minimum: {}",
            enabled(preferences.ntfy_enabled),
            preferences.ntfy_min_severity
        )),
        Line::raw(""),
        Line::styled(
            "Do Not Disturb",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!("window: {dnd}")),
        Line::raw(format!(
            "critical bypass: {}",
            enabled(preferences.critical_bypasses_dnd)
        )),
        Line::raw(""),
        Line::raw(format!("retention: {} days", preferences.retention_days)),
        Line::raw(format!("ntfy server: {}", preferences.ntfy_server)),
        Line::raw(format!(
            "ntfy topic: {}",
            preferences
                .ntfy_topic
                .as_deref()
                .unwrap_or("not configured")
        )),
        Line::raw(format!(
            "muted kinds: {}",
            if preferences.muted_kinds.is_empty() {
                "none".into()
            } else {
                preferences.muted_kinds.join(", ")
            }
        )),
        Line::raw(""),
        Line::styled(
            "Use /notify-severity CHANNEL LEVEL for thresholds.",
            Style::default().fg(Color::DarkGray),
        ),
        Line::styled(
            "Use /dnd HH:MM-HH:MM for an exact UTC window.",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if preferences.ntfy_enabled && preferences.ntfy_topic.is_none() {
        lines.push(Line::styled(
            "ntfy is enabled without a topic; the backend will reject this state.",
            Style::default().fg(Color::Red),
        ));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.notification_preferences_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Notification delivery settings ")
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            "[w] Web  [e] Desktop  [n] ntfy  [q] DND all-day/off",
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::raw("  "),
                        Span::styled("Esc close", Style::default().fg(Color::DarkGray)),
                    ])),
            ),
        area,
    );
}

fn format_utc_minute(value: u16) -> String {
    format!("{:02}:{:02}", value / 60, value % 60)
}

fn render_approval_detail(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(approval) = &app.approval_detail else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(96);
    let height = screen.height.saturating_sub(4).min(28);
    if width < 36 || height < 12 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let status_color = match approval.status.as_str() {
        "pending" => Color::Yellow,
        "approved" => Color::Green,
        "denied" => Color::Red,
        "consumed" => Color::Blue,
        _ => Color::DarkGray,
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                approval.status.to_uppercase(),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(approval.id.clone(), Style::default().fg(Color::DarkGray)),
        ]),
        Line::raw(format!("capability: {}", approval.verb)),
        Line::raw(format!(
            "risk: {}",
            approval.risk.as_deref().unwrap_or("unclassified")
        )),
        Line::raw(format!("session: {}", approval.session)),
        Line::raw(format!("requested: {}", approval.requested_at)),
    ];
    if let Some(requester) = &approval.requester {
        lines.push(Line::raw(format!("requester: {requester}")));
    }
    if let Some(decided_at) = approval.decided_at {
        lines.push(Line::raw(format!("decided: {decided_at}")));
    }
    if let Some(duration) = &approval.duration {
        lines.push(Line::raw(format!("duration: {duration}")));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Scope",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let scope = serde_json::to_string_pretty(&approval.scope)
        .map(|value| clean_text(&value))
        .unwrap_or_else(|_| "[scope unavailable]".into());
    lines.extend(scope.lines().map(|line| Line::raw(line.to_string())));
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Reason",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(markdown_lines(&approval.reason));
    if let Some(note) = &approval.note {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Decision note",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_lines(note));
    }
    let action = if approval.status == "pending" {
        "[a] Approve once  [d] Deny"
    } else {
        "Historical decision - read only"
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.approval_detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(status_color))
                    .title(" Approval center ")
                    .title_bottom(Line::from(vec![
                        Span::styled(action, Style::default().fg(Color::Yellow)),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn render_task_detail(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(task) = &app.task_detail else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(96);
    let height = screen.height.saturating_sub(4).min(28);
    if width < 36 || height < 12 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let status_color = match task.status.as_str() {
        "ok" => Color::Green,
        "error" => Color::Red,
        "cancelled" => Color::DarkGray,
        "waiting_approval" => Color::Magenta,
        "pending" => Color::Yellow,
        _ => Color::Cyan,
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                task.status.to_uppercase(),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(task.id.clone(), Style::default().fg(Color::DarkGray)),
        ]),
        Line::raw(format!("session: {}", task.session_id)),
    ];
    if let Some(workspace) = &task.workspace {
        lines.push(Line::raw(format!("workspace: {workspace}")));
    }
    if let Some(predecessor) = &task.after_task_id {
        lines.push(Line::raw(format!("queued after: {predecessor}")));
    }
    if let Some(activity_id) = &task.activity_id {
        lines.push(Line::raw(format!("activity: {activity_id}")));
    }
    lines.push(Line::raw(format!("created: {}", task.created_at)));
    if let Some(started_at) = &task.started_at {
        lines.push(Line::raw(format!("started: {started_at}")));
    }
    if let Some(finished_at) = &task.finished_at {
        lines.push(Line::raw(format!("finished: {finished_at}")));
    }
    if let Some(model) = task.model.as_ref().or(task.requested_model.as_ref()) {
        lines.push(Line::raw(format!(
            "model: {}{}",
            model,
            task.provider
                .as_deref()
                .map(|provider| format!(" ({provider})"))
                .unwrap_or_default()
        )));
    }
    if let Some(turns) = task.turns_used {
        lines.push(Line::raw(format!("turns: {turns}")));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "Prompt",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(markdown_lines(&task.prompt));
    if let Some(response) = &task.response {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Result",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_lines(response));
    }
    if let Some(error) = &task.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Error",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        lines.extend(error.lines().map(|line| Line::raw(line.to_string())));
    }
    let action = if task.is_terminal() {
        "[r] Retry"
    } else {
        "[c] Cancel"
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.task_detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(status_color))
                    .title(" Durable task ")
                    .title_bottom(Line::from(vec![
                        Span::styled(action, Style::default().fg(Color::Yellow)),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down scroll  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            ),
        area,
    );
}

fn render_confirmation(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(confirmation) = &app.confirmation else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(72);
    let height = 9.min(screen.height.saturating_sub(2));
    if width < 30 || height < 7 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(inline_spans(&confirmation.body)),
            Line::raw(""),
            Line::from(vec![
                Span::styled(
                    format!("[y] {}", confirmation.confirm_label),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("    "),
                Span::styled("[n/Esc] Cancel", Style::default().fg(Color::DarkGray)),
            ]),
        ])
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(format!(" {} ", confirmation.title)),
        ),
        area,
    );
}

fn render_command_palette(frame: &mut Frame<'_>, transcript: Rect, app: &App) {
    let suggestions = suggestions(&app.input);
    if suggestions.is_empty() || app.current_approval().is_some() {
        return;
    }
    let width = transcript.width.saturating_sub(4).min(64);
    let height = (suggestions.len() as u16 + 2)
        .min(transcript.height)
        .min(10);
    if width < 20 || height < 3 {
        return;
    }
    let area = Rect::new(
        transcript.x + 2,
        transcript.bottom().saturating_sub(height),
        width,
        height,
    );
    let max_rows = usize::from(height.saturating_sub(2));
    let selected = app
        .command_selection
        .min(suggestions.len().saturating_sub(1));
    let window_start = selected.saturating_sub(max_rows / 2);
    let command_width = suggestions
        .iter()
        .map(|(command, _)| command.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines = suggestions
        .into_iter()
        .skip(window_start)
        .take(max_rows)
        .enumerate()
        .map(|(index, (command, description))| {
            let selected = window_start + index == selected;
            Line::from(vec![
                Span::styled(
                    format!("{command:<command_width$}"),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    description,
                    Style::default().fg(if selected {
                        Color::White
                    } else {
                        Color::DarkGray
                    }),
                ),
            ])
            .style(if selected {
                Style::default().bg(Color::Rgb(35, 45, 55))
            } else {
                Style::default()
            })
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

fn render_picker(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(picker) = &app.picker else {
        return;
    };
    let indices = app.picker_visible_indices();
    let width = screen.width.saturating_sub(4).min(78);
    let max_rows = screen.height.saturating_sub(8).clamp(4, 12);
    let height = (indices.len() as u16 + 4).max(5).min(max_rows + 4);
    if width < 28 || height < 5 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let selected = picker.selected.min(indices.len().saturating_sub(1));
    let window_start = selected.saturating_sub(max_rows as usize / 2);
    let lines = if indices.is_empty() {
        vec![Line::styled(
            "  No matching items",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        indices
            .iter()
            .skip(window_start)
            .take(max_rows as usize)
            .enumerate()
            .map(|(visible_offset, item_index)| {
                let visible_index = window_start + visible_offset;
                let item = &picker.items[*item_index];
                let selected = visible_index == selected;
                Line::from(vec![
                    Span::styled(
                        if selected { "> " } else { "  " },
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        item.label.clone(),
                        Style::default().fg(Color::White).add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                    ),
                    Span::raw("  "),
                    Span::styled(item.detail.clone(), Style::default().fg(Color::DarkGray)),
                ])
                .style(if selected {
                    Style::default().bg(Color::Rgb(35, 45, 55))
                } else {
                    Style::default()
                })
            })
            .collect::<Vec<_>>()
    };
    let query = if picker.query.is_empty() {
        "type to filter".to_string()
    } else {
        format!("filter: {}", picker.query)
    };
    let title = match picker.kind {
        PickerKind::Models => format!(" {} - model ", picker.title),
        PickerKind::Sessions => format!(" {} - session ", picker.title),
        PickerKind::Tasks => format!(" {} - task ", picker.title),
        PickerKind::Approvals => format!(" {} - approval ", picker.title),
        PickerKind::Notifications => format!(" {} - notification ", picker.title),
        PickerKind::Activities => format!(" {} - Activity ", picker.title),
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title)
                    .title_bottom(Line::from(vec![
                        Span::styled(query, Style::default().fg(Color::DarkGray)),
                        Span::raw("  "),
                        Span::styled(
                            "Up/Down select  Enter choose  Esc close",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let status = match app.status {
        RunStatus::Ready => Span::styled("READY", Style::default().fg(Color::Green)),
        RunStatus::Working => Span::styled(
            format!("{} WORKING {}", spinner(app.frame), elapsed(app)),
            Style::default().fg(Color::Cyan),
        ),
        RunStatus::WaitingApproval => Span::styled("APPROVAL", Style::default().fg(Color::Magenta)),
        RunStatus::Cancelling => Span::styled(
            format!("{} STOPPING", spinner(app.frame)),
            Style::default().fg(Color::Yellow),
        ),
    };
    let short_session = app.conversation.id.chars().take(22).collect::<String>();
    let workspace_path = app
        .active_workspace
        .as_deref()
        .unwrap_or(&app.selected_workspace);
    let workspace = std::path::Path::new(workspace_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace_path);
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
        Span::styled(
            format!("cwd:{workspace}"),
            Style::default().fg(Color::Magenta),
        ),
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

fn spinner(frame: u64) -> &'static str {
    ["|", "/", "-", "\\"][(frame as usize) % 4]
}

fn elapsed(app: &App) -> String {
    app.task_elapsed()
        .map(|elapsed| format!("{:.1}s", elapsed.as_secs_f64()))
        .unwrap_or_default()
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
                let (mark, color, duration) = match status {
                    ToolStatus::Running => ("*", Color::Cyan, None),
                    ToolStatus::Succeeded { duration_ms } => ("+", Color::Green, *duration_ms),
                    ToolStatus::Failed { duration_ms } => ("!", Color::Red, *duration_ms),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(entry.text.clone(), Style::default().fg(Color::Gray)),
                    Span::styled(
                        duration
                            .map(|duration| format!("  {duration}ms"))
                            .unwrap_or_default(),
                        Style::default().fg(Color::DarkGray),
                    ),
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
                let mut spans = vec![Span::styled("  - ", Style::default().fg(Color::Cyan))];
                spans.extend(inline_spans(&line[2..]));
                return Line::from(spans);
            }
            if let Some(quote) = line.strip_prefix("> ") {
                let mut spans = vec![Span::styled("  | ", Style::default().fg(Color::DarkGray))];
                spans.extend(
                    inline_spans(quote).into_iter().map(|span| {
                        span.patch_style(Style::default().add_modifier(Modifier::ITALIC))
                    }),
                );
                return Line::from(spans);
            }
            Line::from(inline_spans(line))
        })
        .collect()
}

fn inline_spans(value: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 2..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::Rgb(35, 35, 35)),
                ));
                rest = &after[end + 1..];
                continue;
            }
        }
        let next = ["**", "`"]
            .into_iter()
            .filter_map(|marker| rest.find(marker))
            .min()
            .unwrap_or(rest.len());
        if next == 0 {
            spans.push(Span::raw(rest[..1].to_string()));
            rest = &rest[1..];
        } else {
            let text = &rest[..next];
            let style = if text.starts_with("http://") || text.starts_with("https://") {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED)
            } else {
                Style::default()
            };
            spans.push(Span::styled(text.to_string(), style));
            rest = &rest[next..];
        }
    }
    spans
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

    let display = app
        .input
        .split('\n')
        .enumerate()
        .map(|(index, line)| format!("{}{}", if index == 0 { "> " } else { "  " }, line))
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(Paragraph::new(display).block(block), area);
    let inner_width = area.width.saturating_sub(4).max(1);
    let before = app.input.chars().take(app.cursor).collect::<String>();
    let before_lines = before.split('\n').collect::<Vec<_>>();
    let current_width =
        UnicodeWidthStr::width(before_lines.last().copied().unwrap_or_default()) as u16;
    let previous_rows = before_lines
        .iter()
        .take(before_lines.len().saturating_sub(1))
        .map(|line| {
            let width = UnicodeWidthStr::width(*line) as u16 + 2;
            width.max(1).div_ceil(inner_width)
        })
        .sum::<u16>();
    let x = area.x + 3 + (current_width % inner_width);
    let y = area.y
        + 1
        + previous_rows
        + ((current_width + 2) / inner_width).min(area.height.saturating_sub(2));
    frame.set_cursor_position(Position::new(
        x.min(area.right().saturating_sub(1)),
        y.min(area.bottom().saturating_sub(1)),
    ));
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let queue = if app.queued_tasks.is_empty() {
        String::new()
    } else {
        format!("  durable queue {}", app.queued_tasks.len())
    };
    let line = Line::from(vec![
        Span::styled(
            " Enter send  Shift+Enter newline  Esc stop  Ctrl+K commands ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(
                "tokens {} in / {} out / {} cached{queue}{}",
                app.usage_input,
                app.usage_output,
                app.usage_cached,
                if app.scroll > 0 {
                    format!("  scroll +{}", app.scroll)
                } else {
                    String::new()
                }
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn composer_height(width: u16, app: &App) -> u16 {
    if app.current_approval().is_some() {
        return 3;
    }
    let inner = width.saturating_sub(4).max(1);
    let rows = app
        .input
        .split('\n')
        .map(|line| {
            let width = UnicodeWidthStr::width(line) as u16 + 2;
            width.max(1).div_ceil(inner)
        })
        .sum::<u16>()
        .clamp(1, 6);
    rows + 2
}

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

mod progress;

use super::commands::suggestions;
use super::state::{
    clean_text, App, ApprovalStatus, EntryKind, PickerKind, RunStatus, TerminalTheme,
};
use progress::spinner;

pub(super) fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let composer_height = composer_height(area.width, app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(if app.active_task.is_some() { 2 } else { 0 }),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
    render_header(frame, chunks[0], app);
    render_transcript(frame, chunks[1], app);
    progress::render(frame, chunks[2], app);
    render_composer(frame, chunks[3], app);
    render_footer(frame, chunks[4], app);
    render_command_palette(frame, chunks[1], app);
    render_picker(frame, area, app);
    render_task_controls(frame, area, app);
    render_appearance(frame, area, app);
    render_voice_overview(frame, area, app);
    render_agents(frame, area, app);
    render_platform_overview(frame, area, app);
    render_memory_center(frame, area, app);
    render_agent_hooks(frame, area, app);
    render_mcp_overview(frame, area, app);
    render_extensions_overview(frame, area, app);
    render_usage_overview(frame, area, app);
    render_debug_overview(frame, area, app);
    render_account_overview(frame, area, app);
    render_task_detail(frame, area, app);
    render_approval_detail(frame, area, app);
    render_notification_detail(frame, area, app);
    render_notification_preferences(frame, area, app);
    render_activity_detail(frame, area, app);
    render_activity_attention(frame, area, app);
    render_activity_controls(frame, area, app);
    render_activity_evidence(frame, area, app);
    render_activity_review(frame, area, app);
    render_activity_operation_preview(frame, area, app);
    render_confirmation(frame, area, app);
}

fn render_platform_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(overview) = &app.platform_overview else {
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
        "Verified read-only inventory - nothing is launched or activated",
        Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
    )];
    lines.extend(
        overview
            .presentation
            .lines()
            .map(|line| Line::raw(line.to_string())),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.platform_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Platform ")
                    .title_bottom(Line::styled(
                        "[m] Mem [h] Hooks [c] MCP [e] Ext [u] Usage [d] Debug [a] Account",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_voice_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(voice) = &app.voice_overview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(88);
    let height = screen.height.saturating_sub(4).min(22);
    if width < 52 || height < 17 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let configured = |value| if value { "configured" } else { "not configured" };
    let lines = vec![
        Line::styled(
            "Claw voice models",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw("Voice uses the OS-configured Claw media providers, not a Codex backend."),
        Line::raw(""),
        Line::styled("Speech to text", Style::default().fg(Color::Yellow)),
        Line::raw(format!(
            "{} / {}  ({})",
            voice.stt_provider,
            voice.stt_model,
            configured(voice.stt_configured)
        )),
        Line::styled("Text to speech", Style::default().fg(Color::Yellow)),
        Line::raw(format!(
            "{} / {}  ({})",
            voice.tts_provider,
            voice.tts_model,
            configured(voice.tts_configured)
        )),
        Line::raw(format!(
            "voice {}  output format {}",
            voice.tts_voice, voice.tts_format
        )),
        Line::raw(""),
        Line::styled("Realtime conversation", Style::default().fg(Color::Yellow)),
        Line::raw(if voice.realtime_capture_available {
            "Capability-gated microphone capture is available."
        } else {
            "Unavailable: this build has no capability-gated microphone recorder/session service."
        }),
        Line::raw("The TUI will not open PipeWire or microphone devices directly."),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Voice Center ")
                    .title_bottom(Line::styled(
                        "Esc close",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_memory_center(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    if !app.memory_center_open {
        return;
    }
    let width = screen.width.saturating_sub(4).min(88);
    let height = screen.height.saturating_sub(4).min(20);
    if width < 48 || height < 16 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let memory = if app.task_use_memory { "on" } else { "off" };
    let lines = vec![
        Line::styled(
            "Memory settings",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            format!("[m] Use and record memory for future tasks: {memory}"),
            Style::default().fg(Color::Yellow),
        ),
        Line::raw(
            "    Loads retained conversation context and records new Agent memory for each future task.",
        ),
        Line::raw(
            "    Turning it off does not remove durable Job metadata or audit evidence.",
        ),
        Line::raw(""),
        Line::styled("[r] Reset learned memory", Style::default().fg(Color::Yellow)),
        Line::raw(
            "    Deletes notes, App-emitted memory, and the derived semantic index after confirmation.",
        ),
        Line::raw(
            "    Conversations, Jobs, task bindings, compaction summaries, and audit remain.",
        ),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(accent(app)))
                .title(" Memory Center ")
                .title_bottom(Line::styled(
                    "[m] toggle  [r] reset  Esc back",
                    Style::default().fg(Color::DarkGray),
                )),
        ),
        area,
    );
}

fn render_agent_hooks(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(settings) = &app.agent_hook_settings else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(88);
    let height = screen.height.saturating_sub(4).min(21);
    if width < 48 || height < 17 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let enabled = |value| if value { "on" } else { "off" };
    let mut lines = vec![
        Line::styled(
            "Built-in lifecycle hooks - future tasks only",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw("No arbitrary commands or environment values are accepted."),
        Line::raw(""),
        Line::styled(
            format!("[l] logging: {}", enabled(settings.logging)),
            Style::default().fg(Color::Yellow),
        ),
        Line::raw("    Emits structured lifecycle events to Claw tracing."),
        Line::styled(
            format!("[a] audit: {}", enabled(settings.audit)),
            Style::default().fg(Color::Yellow),
        ),
        Line::raw("    Adds owner Agent lifecycle records; broker audit remains mandatory."),
        Line::styled(
            format!("[c] checkpoint: {}", enabled(settings.checkpoint)),
            Style::default().fg(Color::Yellow),
        ),
        Line::raw("    Creates checkpoints before configured dangerous tools."),
        Line::raw(""),
        Line::styled(
            "Changes are loaded by future tasks and never rewrite a running task.",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let (Some(kind), Some(changed)) = (&settings.updated_kind, settings.changed) {
        let state = app.agent_hook_enabled(kind).map(enabled).unwrap_or("unknown");
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            if changed {
                format!("Saved {kind}: {state}.")
            } else {
                format!("{kind} was already {state}.")
            },
            Style::default().fg(accent(app)),
        ));
    }
    if let Some(error) = &app.agent_hook_error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(error.clone(), Style::default().fg(Color::Red)));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(accent(app)))
                .title(" Hooks Center ")
                .title_bottom(Line::styled(
                    "[l] logging  [a] audit  [c] checkpoint  Esc back",
                    Style::default().fg(Color::DarkGray),
                )),
        ),
        area,
    );
}

fn render_mcp_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(overview) = &app.mcp_overview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(94);
    let height = screen.height.saturating_sub(4).min(30);
    if width < 52 || height < 16 {
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
            "MCP server inventory - read only",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw("No server is started or probed; command, args, env, cwd, and URL stay hidden."),
        Line::raw(format!(
            "verified discovery: {}",
            if overview.discovery_enabled { "on" } else { "off" }
        )),
        Line::raw(""),
    ];
    if overview.servers.is_empty() {
        lines.push(Line::styled(
            "No configured or verified discovered MCP server.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for server in &overview.servers {
            let status = if server.enabled { "enabled" } else { "disabled" };
            let timeout = if server.timeout_secs == 0 {
                "no timeout".to_string()
            } else {
                format!("{}s timeout", server.timeout_secs)
            };
            lines.push(Line::raw(format!(
                "- {status:<8} {}  [{}; {timeout}]",
                server.name, server.transport
            )));
            lines.push(Line::styled(
                format!("  {}", server.source),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    if overview.truncated {
        lines.push(Line::styled(
            "Inventory truncated at 256 entries.",
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.mcp_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" MCP Center ")
                    .title_bottom(Line::styled(
                        "Up/Down scroll  Esc back",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_extensions_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(overview) = &app.extensions_overview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(100);
    let height = screen.height.saturating_sub(4).min(32);
    if width < 56 || height < 16 {
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
            "One authenticated Claw extension model",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw("Apps, Skills, and Agent extensions share one model; MCP stays in MCP Center."),
        Line::raw("Claw has no separate Plugin authority."),
        Line::raw("This inventory executes and activates nothing; package changes stay in OS review flows."),
        Line::raw(""),
    ];
    if overview.entries.is_empty() {
        lines.push(Line::styled(
            "No authenticated or quarantined extension entry is installed.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for entry in &overview.entries {
            let color = if entry.status == "quarantined" {
                Color::Red
            } else if entry.status == "disabled" {
                Color::Yellow
            } else {
                Color::Green
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<15}", entry.kind),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<12}", entry.status),
                    Style::default().fg(color),
                ),
                Span::raw(format!("{} [{}]", entry.id, entry.trust)),
            ]));
            if let Some(diagnostic) = &entry.diagnostic {
                lines.push(Line::styled(
                    format!("  {diagnostic}"),
                    Style::default().fg(color),
                ));
            }
        }
    }
    if overview.truncated {
        lines.push(Line::styled(
            "Inventory truncated at 512 entries.",
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.extensions_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Extensions Center ")
                    .title_bottom(Line::styled(
                        "Up/Down scroll  Esc back",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_usage_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(overview) = &app.usage_overview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(96);
    let height = screen.height.saturating_sub(4).min(32);
    if width < 56 || height < 18 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let total = &overview.total;
    let mut lines = vec![
        Line::styled(
            format!("Owner Agent usage - {}", overview.period.label()),
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw("Canonical ledger totals only; Claw does not infer monetary cost."),
        Line::raw(""),
        Line::raw(format!(
            "calls {}  success {}  error {}  duration {} ms",
            total.calls, total.success, total.error, total.total_duration_ms
        )),
        Line::raw(format!(
            "tokens: input {}  output {}  cache read {}  cache write {}",
            total.input_tokens,
            total.output_tokens,
            total.cache_read_tokens,
            total.cache_write_tokens
        )),
        Line::raw(format!(
            "ledger: {} lines / {} bytes  parse errors {}",
            overview.log_lines, overview.log_bytes, overview.parse_errors
        )),
        Line::raw(""),
        Line::styled("Providers", Style::default().fg(Color::Yellow)),
    ];
    append_usage_breakdown(&mut lines, &overview.providers);
    lines.push(Line::raw(""));
    lines.push(Line::styled("Models", Style::default().fg(Color::Yellow)));
    append_usage_breakdown(&mut lines, &overview.models);
    if overview.breakdown_truncated {
        lines.push(Line::styled(
            "Breakdown truncated by the canonical ledger or terminal bound.",
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.usage_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Usage Center ")
                    .title_bottom(Line::styled(
                        "[d] daily  [w] weekly  [c] cumulative  Up/Down scroll  Esc back",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_debug_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(debug) = &app.debug_overview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(96);
    let height = screen.height.saturating_sub(4).min(31);
    if width < 56 || height < 18 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let on = |value| if value { "on" } else { "off" };
    let allow = debug
        .tool_allow_count
        .map_or_else(|| "all minus deny list".to_string(), |count| format!("{count} names"));
    let active_task = app.active_task.as_deref().unwrap_or("none");
    let reasoning = debug.reasoning_effort.as_deref().unwrap_or("provider default");
    let mut lines = vec![
        Line::styled(
            "Safe effective runtime diagnostics",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw("Credentials, headers, URLs, MCP launch data, and private payloads are omitted."),
        Line::raw(""),
        Line::styled("Broker", Style::default().fg(Color::Yellow)),
        Line::raw(format!(
            "{}: {}  uptime {} ms  started {}",
            debug.daemon, debug.daemon_status, debug.uptime_ms, debug.started_at
        )),
        Line::styled("Conversation", Style::default().fg(Color::Yellow)),
        Line::raw(format!("id: {}", app.conversation.id)),
        Line::raw(format!("workspace: {}", clean_text(&app.selected_workspace))),
        Line::raw(format!("active task: {active_task}")),
        Line::styled("Provider", Style::default().fg(Color::Yellow)),
        Line::raw(format!(
            "{} / {}  ready {}  catalogue {} model(s)",
            debug.provider,
            debug.model,
            on(debug.provider_ready),
            debug.model_count
        )),
        Line::raw(format!(
            "max turns {}  reasoning {}  compression {}",
            debug.max_turns,
            reasoning,
            on(debug.compression_enabled)
        )),
        Line::styled("Safety and extensions", Style::default().fg(Color::Yellow)),
        Line::raw(format!(
            "memory redaction {}  progressive tools {}",
            on(debug.memory_redaction_enabled),
            on(debug.progressive_tools_enabled)
        )),
        Line::raw(format!(
            "tool allow: {allow}  deny: {}",
            debug.tool_deny_count
        )),
        Line::raw(format!(
            "configured MCP: {}  discovery {}  selected Agent extensions: {}",
            debug.configured_mcp_count,
            on(debug.mcp_discovery_enabled),
            debug.selected_extension_count
        )),
    ];
    if let Some(warning) = &debug.model_catalog_warning {
        lines.push(Line::styled(
            format!("model catalogue warning: {warning}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.debug_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Debug Center ")
                    .title_bottom(Line::styled(
                        "Up/Down scroll  Esc back",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn render_account_overview(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(account) = &app.account_overview else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(88);
    let height = screen.height.saturating_sub(4).min(22);
    if width < 52 || height < 17 {
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
            "Agent account and import",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!("configured provider: {}", app.info.provider)),
        Line::raw(format!(
            "{} credential: {}",
            account.provider,
            if account.credential_present {
                "present (validity checked only when used)"
            } else {
                "not present"
            }
        )),
        Line::raw(""),
        Line::styled("Logout", Style::default().fg(Color::Yellow)),
    ];
    if account.credential_present {
        lines.push(Line::raw("[l] Revoke the owner Copilot credential after confirmation."));
    } else {
        lines.push(Line::styled(
            "No stored Copilot credential to revoke.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.extend([
        Line::raw("Provider configuration and conversation history are never deleted by logout."),
        Line::raw(""),
        Line::styled("Import", Style::default().fg(Color::Yellow)),
        Line::raw("No authenticated foreign-agent importer is registered in this Claw build."),
        Line::raw("The renderer will not scan Claude, Cursor, Codex, or other App data roots."),
        Line::raw("Use an explicit OS-reviewed import service when one is installed."),
    ]);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Account Center ")
                    .title_bottom(Line::styled(
                        if account.credential_present {
                            "[l] logout  Esc back"
                        } else {
                            "Esc back"
                        },
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn append_usage_breakdown(
    lines: &mut Vec<Line<'static>>,
    entries: &[super::backend::UsageBreakdown],
) {
    if entries.is_empty() {
        lines.push(Line::styled("  none", Style::default().fg(Color::DarkGray)));
        return;
    }
    lines.extend(entries.iter().map(|entry| {
        Line::raw(format!(
            "  {}: {} calls, {} in / {} out",
            entry.name, entry.totals.calls, entry.totals.input_tokens, entry.totals.output_tokens
        ))
    }));
}

fn render_agents(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    if !app.agents_open {
        return;
    }
    let width = screen.width.saturating_sub(4).min(84);
    let height = screen.height.saturating_sub(4).min(24);
    if width < 40 || height < 10 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let summaries = app.delegate_summaries();
    let mut lines = vec![
        Line::styled(
            "Scoped delegate calls recorded in the parent task",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "Delegates are ephemeral child loops, not switchable durable sessions.",
            Style::default().fg(Color::DarkGray),
        ),
        Line::raw(""),
    ];
    if summaries.is_empty() {
        lines.push(Line::styled(
            "No cos_delegate calls are present in this transcript.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.extend(summaries.into_iter().map(|(id, status)| {
            Line::raw(format!("- {status:<9} {id}"))
        }));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.agents_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
                    .title(" Agents ")
                    .title_bottom(Line::styled(
                        "Up/Down scroll  Esc close",
                        Style::default().fg(Color::DarkGray),
                    )),
            ),
        area,
    );
}

fn accent(app: &App) -> Color {
    match app.terminal_theme {
        TerminalTheme::Cyan => Color::Cyan,
        TerminalTheme::Blue => Color::Blue,
        TerminalTheme::Magenta => Color::Magenta,
    }
}

fn render_appearance(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    if !app.appearance_open {
        return;
    }
    let width = screen.width.saturating_sub(4).min(68);
    let height = 11.min(screen.height.saturating_sub(4));
    if width < 36 || height < 9 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let lines = vec![
        Line::styled(
            "Local terminal presentation only",
            Style::default().fg(accent(app)).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw(format!("theme: {}", app.theme_name())),
        Line::raw(format!(
            "terminal title: {}",
            if app.terminal_title_enabled { "on" } else { "off" }
        )),
        Line::raw(format!(
            "status line: {}",
            if app.compact_statusline { "compact" } else { "full" }
        )),
        Line::raw(format!("keymap: {}", app.keymap_name())),
        Line::raw(""),
        Line::styled(
            "No provider, task, permission, or persisted state is changed.",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(accent(app)))
                .title(" Appearance ")
                .title_bottom(Line::from(vec![
                    Span::styled(
                        "[t] Theme  [h] Title  [s] Status  [k] Keymap",
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw("  "),
                    Span::styled("Esc close", Style::default().fg(Color::DarkGray)),
                ])),
        ),
        area,
    );
}

fn render_task_controls(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    if !app.task_controls_open {
        return;
    }
    let width = screen.width.saturating_sub(4).min(72);
    let height = 12.min(screen.height.saturating_sub(4));
    if width < 38 || height < 10 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width.saturating_sub(width)) / 2,
        screen.y + (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    let lines = vec![
        Line::styled(
            "Defaults for future tasks only",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw(format!("model: {}", app.selected_model)),
        Line::raw(format!(
            "memory: {}",
            if app.task_use_memory { "on" } else { "off" }
        )),
        Line::raw(format!(
            "max turns: {}",
            app.task_max_turns
                .map(|turns| turns.to_string())
                .unwrap_or_else(|| "configured default".into())
        )),
        Line::raw(format!(
            "reasoning effort: {}",
            app.task_reasoning_effort
                .as_deref()
                .unwrap_or("provider default")
        )),
        Line::raw(format!(
            "mode: {}",
            if app.task_plan_only { "plan only" } else { "execute" }
        )),
        Line::raw(""),
        Line::styled(
            "These settings do not change credentials, provider, or global config.",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(" Task model controls ")
                .title_bottom(Line::from(vec![
                    Span::styled(
                        "[o] Model  [r] Reasoning  [p] Plan  [m] Memory  [t] Turns",
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw("  "),
                    Span::styled("Esc close", Style::default().fg(Color::DarkGray)),
                ])),
        ),
        area,
    );
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
                            "[p] Prepare operation preview  [v] Review file plans",
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

fn render_activity_review(frame: &mut Frame<'_>, screen: Rect, app: &App) {
    let Some(review) = &app.activity_review else {
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
        "App-reported proposals only - review grants no authority and applies nothing",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )];
    lines.extend(
        review
            .presentation
            .lines()
            .map(|line| Line::raw(line.to_string())),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.activity_review_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue))
                    .title(" Staged file review ")
                    .title_bottom(Line::from(vec![
                        Span::styled("[e] Full evidence", Style::default().fg(Color::Yellow)),
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
            "[r] Run  [p] Pause  [c] Complete  [x] Cancel  [a] Attention  [o] Controls  [e] Evidence  [v] Review"
        }
        "paused" => {
            "[u] Resume  [c] Complete  [x] Cancel  [a] Attention  [o] Controls  [e] Evidence  [v] Review"
        }
        "completed" | "cancelled" => {
            "[u] Reopen  [a] Attention  [o] Controls  [e] Evidence  [v] Review"
        }
        _ => "[a] Attention  [o] Controls  [e] Evidence  [v] Review",
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
                    .border_style(Style::default().fg(accent(app)))
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
    if let Some(effort) = &task.requested_reasoning_effort {
        lines.push(Line::raw(format!("reasoning effort: {effort}")));
    }
    if task.plan_only {
        lines.push(Line::raw("mode: plan only (tools disabled)"));
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
                    Style::default().fg(accent(app)),
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
                        Style::default().fg(accent(app)),
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
        PickerKind::History => format!(" {} - history ", picker.title),
        PickerKind::Files => format!(" {} - file ", picker.title),
        PickerKind::Tasks => format!(" {} - task ", picker.title),
        PickerKind::Approvals => format!(" {} - approval ", picker.title),
        PickerKind::Notifications => format!(" {} - notification ", picker.title),
        PickerKind::Activities => format!(" {} - Activity ", picker.title),
        PickerKind::ActivityReviews => format!(" {} - review ", picker.title),
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent(app)))
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
            Style::default().fg(accent(app)),
        ),
        RunStatus::Reconnecting => Span::styled(
            format!("{} RECONNECTING", spinner(app.frame)),
            Style::default().fg(Color::Yellow),
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
                .bg(accent(app))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        status,
        Span::styled(
            if app.in_side_conversation() {
                "  SIDE"
            } else {
                ""
            },
            Style::default().fg(Color::Yellow),
        ),
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

fn elapsed(app: &App) -> String {
    app.task_elapsed()
        .map(|elapsed| format!("{:.1}s", elapsed.as_secs_f64()))
        .unwrap_or_default()
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let lines = transcript_lines(app);
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

fn transcript_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for entry in &app.entries {
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
                lines.push(progress::tool_line(&entry.text, *status, app));
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
    let title = if app.current_approval().is_some() {
        " Permission "
    } else if app.vim_mode {
        if app.vim_insert_mode {
            " Message - VIM INSERT "
        } else {
            " Message - VIM NORMAL "
        }
    } else {
        " Message "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.current_approval().is_some() {
            Color::Magenta
        } else {
            accent(app)
        }))
        .title(title);
    if let Some(approval) = app.current_approval() {
        let choice_is_explicit = app.approval_choice_is_explicit();
        let approve_style = if choice_is_explicit
            && app.approval_choice == super::backend::ReviewDecision::ApproveOnce
        {
            Style::default()
                .fg(Color::White)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let deny_selected =
            choice_is_explicit && app.approval_choice == super::backend::ReviewDecision::Deny;
        let deny_style = if deny_selected {
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let text = vec![
            Line::from(vec![
                Span::styled(
                    format!("{} {} ", approval.verb, approval.scope),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Authorize once ", approve_style),
                Span::raw("  "),
                Span::styled(" Deny ", deny_style),
                Span::styled(
                    "   Left/Right choose  Enter confirm  a/d direct  Esc/Ctrl+C stop task",
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ];
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
    let attachments = if app.pending_attachment_count() == 0 {
        String::new()
    } else {
        format!("  images {}", app.pending_attachment_count())
    };
    let controls = if app.backtrack_armed() {
        " Esc again history  Ctrl+K commands "
    } else {
        " Enter send  Ctrl+J newline  Ctrl+F file  Ctrl+O image  Ctrl+T model  Super+Shift+A voice  Esc stop "
    };
    let usage = format!(
        "tokens {} in / {} out / {} cached{queue}{}",
        app.usage_input,
        app.usage_output,
        app.usage_cached,
        if app.scroll > 0 {
            format!("  scroll +{}", app.scroll)
        } else {
            String::new()
        },
    );
    let line = Line::from(vec![
        Span::styled(
            controls,
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            if app.compact_statusline {
                String::new()
            } else {
                usage
            },
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(attachments, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn composer_height(width: u16, app: &App) -> u16 {
    if app.current_approval().is_some() {
        return 4;
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

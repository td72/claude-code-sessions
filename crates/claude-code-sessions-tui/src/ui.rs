//! Drawing: a session list and a detail pane for the selected session.

use claude_code_sessions::record::Timestamp;
use claude_code_sessions::session::{LiveState, ModelUsage, Session};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Gauge, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::{self, App, LiveStatus, StatusKind};
use crate::format;

/// Below this width the detail pane goes under the list instead of beside it.
const SIDE_BY_SIDE_FROM: u16 = 110;

pub fn draw(frame: &mut Frame, app: &App, now: Timestamp) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app, now);
    let [list, detail] = if body.width >= SIDE_BY_SIDE_FROM {
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(body)
    } else {
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body)
    };
    draw_list(frame, list, app, now);
    draw_detail(frame, detail, app, now);
    draw_footer(frame, footer, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, now: Timestamp) {
    let mut counts = [0usize; 3];
    for session in app.store.sessions() {
        match app::status(session, now).kind {
            StatusKind::Busy => counts[0] += 1,
            StatusKind::NeedsApproval => counts[1] += 1,
            StatusKind::Waiting => counts[2] += 1,
            StatusKind::Idle | StatusKind::Exited => {}
        }
    }
    let live = match &app.live {
        LiveStatus::Off => Span::styled("live off", dim()),
        LiveStatus::Listening(_) => Span::styled("live on", Style::new().fg(Color::Green)),
        LiveStatus::Failed(_) => Span::styled("live failed", Style::new().fg(Color::Red)),
    };
    let mut spans = vec![
        Span::styled(
            "claude-code-sessions",
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {} sessions", app.order.len())),
        Span::raw(format!("  {} busy", counts[0])),
    ];
    if counts[1] > 0 {
        spans.push(Span::styled(
            format!("  {} need approval", counts[1]),
            style_of(StatusKind::NeedsApproval),
        ));
    }
    if counts[2] > 0 {
        spans.push(Span::styled(
            format!("  {} waiting", counts[2]),
            style_of(StatusKind::Waiting),
        ));
    }
    spans.push(Span::raw("  "));
    spans.push(live);
    frame.render_widget(Line::from(spans), area);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &App, now: Timestamp) {
    let rows = app
        .order
        .iter()
        .filter_map(|id| app.store.get(id))
        .map(|session| {
            let status = app::status(session, now);
            let updated = app::latest(session).map_or("—".to_owned(), |at| format::ago(now, at));
            Row::new([
                Cell::from(
                    app::label(session)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                ),
                Cell::from(app::project(session).unwrap_or_else(|| "—".to_owned())),
                Cell::from(session.git_branch.clone().unwrap_or_else(|| "—".to_owned())),
                Cell::from(status.text).style(style_of(status.kind)),
                Cell::from(Line::from(updated).right_aligned()),
                Cell::from(Line::from(format::cost(&session.cost)).right_aligned()),
            ])
        });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(2),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(16),
            Constraint::Length(7),
            Constraint::Length(9),
        ],
    )
    .header(Row::new(["title", "repository", "branch", "state", "updated", "cost"]).style(dim()))
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .block(Block::bordered().title(" sessions "));
    let mut state =
        TableState::default().with_selected((!app.order.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App, now: Timestamp) {
    let block = Block::bordered().title(" session ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(session) = app.selected_session() else {
        let message = app.notice.as_deref().unwrap_or("no sessions");
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: true }), inner);
        return;
    };

    let [gauge_area, text_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    draw_context(frame, gauge_area, session);
    frame.render_widget(
        Paragraph::new(detail_lines(session, now, text_area.width as usize)),
        text_area,
    );
}

fn draw_context(frame: &mut Frame, area: Rect, session: &Session) {
    let Some(context) = session.current_context() else {
        frame.render_widget(Line::styled("context —", dim()), area);
        return;
    };
    let Some(fill) = context.fill() else {
        let text = format!(
            "context {} of an unknown window",
            format::tokens(context.tokens)
        );
        frame.render_widget(Line::styled(text, dim()), area);
        return;
    };
    let color = match fill {
        f if f >= 0.9 => Color::Red,
        f if f >= 0.7 => Color::Yellow,
        _ => Color::Green,
    };
    let label = format!(
        "context {} of {} ({})",
        format::tokens(context.tokens),
        format::tokens(context.window.unwrap_or_default()),
        format::percent(fill)
    );
    let gauge = Gauge::default()
        .ratio(fill.clamp(0.0, 1.0))
        .label(label)
        .gauge_style(Style::new().fg(color));
    frame.render_widget(gauge, area);
}

fn detail_lines(session: &Session, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let field = |name: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{name:<9}"), dim()),
            Span::raw(value),
        ])
    };
    let status = app::status(session, now);

    lines.push(Line::styled(
        format::truncate(app::label(session), width),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<9}", "state"), dim()),
        Span::styled(status.text, style_of(status.kind)),
    ]));
    if let Some(live) = &session.live {
        if let LiveState::RunningTool(tool) = live.state() {
            if let Some(summary) = &tool.summary {
                lines.push(field(
                    "",
                    format::truncate(summary, width.saturating_sub(9)),
                ));
            }
        }
    }
    let path = session.project_path.as_ref().or(session.cwd.as_ref());
    if let Some(path) = path {
        lines.push(field("path", path.display().to_string()));
    }
    if let Some(branch) = &session.git_branch {
        lines.push(field("branch", branch.clone()));
    }
    if let Some(model) = &session.model {
        lines.push(field("model", model.clone()));
    }
    lines.push(field(
        "activity",
        format!(
            "{}, {}",
            format::count(session.turns, "turn"),
            format::count(session.tool_calls, "tool call")
        ),
    ));
    if let Some(prompt) = &session.last_prompt {
        lines.push(field(
            "prompt",
            format::truncate(prompt, width.saturating_sub(9)),
        ));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "{:<22}{:>9}{:>9}{:>9}{:>10}",
            "tokens and cost", "in", "out", "cache", "cost"
        ),
        dim(),
    ));
    for (model, usage) in &session.usage.by_model {
        lines.push(usage_line(model, usage));
    }
    let total = session.usage.total();
    if session.usage.by_model.len() != 1 {
        lines.push(usage_line("total", &total));
    }
    lines.push(field(
        "session",
        format::cost(&session.cost) + cost_note(session),
    ));

    if let Some(live) = &session.live {
        if !live.rate_limits.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("rate limits", dim()));
            for limit in &live.rate_limits {
                lines.push(Line::raw(format!(
                    "  {:<14}{:>4.0}%",
                    limit.kind, limit.percent_used
                )));
            }
        }
    }

    if !session.recent_tools.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("recent tool calls", dim()));
        // The store keeps them in the order they were read; show the latest
        // first.
        let mut calls: Vec<_> = session.recent_tools.iter().collect();
        calls.sort_by_key(|call| std::cmp::Reverse(call.started_at));
        for call in calls.into_iter().take(8) {
            let (mark, style) = match call.success {
                Some(true) => ("ok", Style::new().fg(Color::Green)),
                Some(false) => ("failed", Style::new().fg(Color::Red)),
                None => ("…", dim()),
            };
            let took = call.duration_ms.map(format::duration).unwrap_or_default();
            let who = if call.agent_id.is_some() {
                " (subagent)"
            } else {
                ""
            };
            lines.push(Line::from(vec![
                Span::raw(format!(
                    "  {:<28}",
                    format::truncate(&format!("{}{who}", call.name), 28)
                )),
                Span::raw(format!("{took:>8}  ")),
                Span::styled(mark, style),
            ]));
        }
    }

    if !session.subagents.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("subagents", dim()));
        for subagent in session.subagents.values() {
            let name = subagent
                .description
                .as_deref()
                .or(subagent.agent_type.as_deref())
                .unwrap_or(&subagent.agent_id);
            let kind = subagent.agent_type.as_deref().unwrap_or("agent");
            let done = match subagent.finished {
                Some(true) => "done",
                _ => "",
            };
            lines.push(Line::raw(format!(
                "  {:<10}{:<24}{:>8}{:>10}  {done}",
                format::truncate(kind, 9),
                format::truncate(name, 23),
                format::tokens(subagent.usage.total_tokens()),
                subagent
                    .usage
                    .estimated_cost()
                    .map_or("—".to_owned(), |usd| format!(
                        "~{}",
                        format::usd_amount(usd)
                    )),
            )));
        }
    }
    lines
}

fn usage_line(name: &str, usage: &ModelUsage) -> Line<'static> {
    let cache = usage.cache_write_5m_tokens + usage.cache_write_1h_tokens + usage.cache_read_tokens;
    Line::raw(format!(
        "  {:<20}{:>9}{:>9}{:>9}{:>10}",
        format::truncate(name, 20),
        format::tokens(usage.input_tokens),
        format::tokens(usage.output_tokens),
        format::tokens(cache),
        usage.estimated_cost().map_or("—".to_owned(), |usd| format!(
            "~{}",
            format::usd_amount(usd)
        )),
    ))
}

/// Where the session's cost came from, in words.
fn cost_note(session: &Session) -> &'static str {
    use claude_code_sessions::session::CostSource;
    match session.cost.source {
        CostSource::Unknown => "",
        CostSource::Reported => "  reported by Claude Code",
        CostSource::ReportedPlusEstimate => "  reported, plus an estimate since",
        CostSource::Estimated => "  estimated from the price table",
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match (&app.notice, &app.live) {
        (Some(notice), _) => Line::styled(notice.clone(), Style::new().fg(Color::Red)),
        (None, LiveStatus::Failed(reason)) => Line::styled(
            format!("live events unavailable: {reason}"),
            Style::new().fg(Color::Red),
        ),
        _ => Line::styled(
            "↑/↓ j/k select   g/G first/last   r refresh   q quit",
            dim(),
        ),
    };
    frame.render_widget(line, area);
}

fn style_of(kind: StatusKind) -> Style {
    match kind {
        StatusKind::Busy => Style::new().fg(Color::Green),
        StatusKind::NeedsApproval => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        StatusKind::Waiting => Style::new().fg(Color::Cyan),
        StatusKind::Idle => Style::new(),
        StatusKind::Exited => dim(),
    }
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::fixture_app;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Draws the app and returns the screen as text, one line per row.
    fn screen(app: &App, width: u16, height: u16, now: &str) -> String {
        let now = Timestamp::parse(now).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_list_and_the_detail_pane_show_the_fixture_sessions() {
        let mut app = fixture_app();
        app.select_last(); // the plain turn, the oldest
        let screen = screen(&app, 150, 40, "2026-11-03T12:00:00.000Z");

        // Header and list.
        assert!(screen.contains("3 sessions"), "{screen}");
        assert!(screen.contains("live off"));
        assert!(screen.contains("Synthetic session title"));
        // Cut by its column.
        assert!(screen.contains("Synthetic title after"));
        assert!(screen.contains("other.repo"));
        assert!(screen.contains("main"));
        assert!(screen.contains("idle"));
        assert!(screen.contains("~$0.0089"));

        // Detail of the selected session.
        assert!(screen.contains("claude-fable-5-2"));
        assert!(screen.contains("/work/example"));
        assert!(screen.contains("1 turn, 0 tool calls"));
        assert!(screen.contains("Synthetic last prompt"));
        assert!(screen.contains("context 4.3k of 1.0M (0%)"));
        assert!(screen.contains("estimated from the price table"));
        assert!(screen.contains("q quit"));
    }

    #[test]
    fn tool_calls_and_subagents_are_listed() {
        let mut app = fixture_app();
        app.select_first();
        app.select_next(); // the session with subagents
        let screen = screen(&app, 150, 45, "2026-11-02T10:00:20.000Z");
        assert!(screen.contains("recent tool calls"), "{screen}");
        assert!(screen.contains("Bash"));
        assert!(screen.contains("1.5s"));
        assert!(screen.contains("failed"));
        assert!(screen.contains("Grep (subagent)"));
        assert!(screen.contains("subagents"));
        assert!(screen.contains("Explore"));
        assert!(screen.contains("Synthetic agent task"));
        assert!(screen.contains("claude-haiku-5-5"));
        assert!(screen.contains("total"));
        // A record 12 seconds ago: active.
        assert!(screen.contains("active"));
    }

    #[test]
    fn a_narrow_terminal_stacks_the_panes_and_nothing_panics_when_tiny() {
        let app = fixture_app();
        let narrow = screen(&app, 80, 40, "2026-11-03T12:00:00.000Z");
        assert!(narrow.contains("sessions"));
        assert!(narrow.contains("session "));
        for (width, height) in [(1, 1), (10, 3), (30, 5), (200, 2)] {
            screen(&app, width, height, "2026-11-03T12:00:00.000Z");
        }
    }

    #[test]
    #[ignore = "prints the screen for a human to look at"]
    fn show_the_screen() {
        let mut app = fixture_app();
        app.select_next();
        println!("{}", screen(&app, 150, 40, "2026-11-02T10:00:20.000Z"));
        println!("{}", screen(&app, 80, 36, "2026-11-02T10:00:20.000Z"));
    }

    #[test]
    fn an_empty_store_shows_the_notice() {
        let app = App::new(crate::app::Config {
            projects_dir: crate::app::tests::fixtures().join("no-such-directory"),
            live: false,
            socket_dir: None,
        });
        let screen = screen(&app, 120, 20, "2026-11-03T12:00:00.000Z");
        assert!(screen.contains("0 sessions"), "{screen}");
        assert!(screen.contains("no projects directory"));
    }

    #[test]
    fn no_unmodelled_text_reaches_the_screen() {
        let mut app = fixture_app();
        for _ in 0..3 {
            let screen = screen(&app, 160, 50, "2026-11-03T12:00:00.000Z");
            assert!(!screen.contains("SYNTHETIC-TEXT"), "{screen}");
            app.select_next();
        }
    }
}

//! Drawing. The Sessions page is three lists over a detail pane, in fixed
//! proportions (the shape of vig's pages); the Usage page is three tables.

use std::collections::BTreeMap;

use iktinos::record::Timestamp;
use iktinos::session::{CostSource, LiveState, ModelUsage, Session, Subagent, ToolCall};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::{self, App, LiveStatus, Page, Pane, StatusKind};
use crate::format;

pub fn draw(frame: &mut Frame, app: &mut App, now: Timestamp) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app, now);
    match app.page {
        Page::Sessions => draw_sessions_page(frame, body, app, now),
        Page::Usage => draw_usage_page(frame, body, app),
    }
    draw_footer(frame, footer, app);
    if app.help {
        draw_help(frame, frame.area());
    }
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
    let tab = |page: Page, text: &'static str| {
        let style = if app.page == page {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            dim()
        };
        Span::styled(text, style)
    };
    let mut spans = vec![
        Span::raw(" "),
        tab(Page::Sessions, " 1:Sessions "),
        Span::raw(" "),
        tab(Page::Usage, " 2:Usage "),
        Span::raw(format!(
            "   {} sessions  {} busy",
            app.store.len(),
            counts[0]
        )),
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
    spans.push(match &app.live {
        LiveStatus::Off => Span::styled("live off", dim()),
        LiveStatus::Listening(_) => Span::styled("live on", Style::new().fg(Color::Green)),
        LiveStatus::Failed(_) => Span::styled("live failed", Style::new().fg(Color::Red)),
    });
    frame.render_widget(Line::from(spans), area);
}

/// A pane's frame: its title, the search in force and whether it has focus.
fn pane_block(app: &App, pane: Pane, title: String, matches: usize) -> Block<'static> {
    let focused = app.focus == pane;
    let mut title = format!(" {title} ");
    if let Some(search) = app.search.as_ref().filter(|s| s.pane == pane) {
        title.push_str(&format!("/{} ({matches}) ", search.query));
    }
    let style = if focused {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        dim()
    };
    Block::bordered().title(title).border_style(style)
}

fn draw_sessions_page(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    // Fixed proportions: the lists on top, the detail below.
    let [upper, lower] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Min(3)]).areas(area);
    let [sessions, tools, subagents] = Layout::horizontal([
        Constraint::Percentage(46),
        Constraint::Percentage(30),
        Constraint::Min(10),
    ])
    .areas(upper);

    app.viewport.list_rows = (sessions.height as usize).saturating_sub(3);
    draw_session_list(frame, sessions, app, now);
    draw_tool_list(frame, tools, app, now);
    draw_subagent_list(frame, subagents, app, now);
    draw_detail(frame, lower, app, now);
}

/// The style of a row the search matches.
fn row_style(matches: &[usize], index: usize) -> Style {
    if matches.contains(&index) {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new()
    }
}

fn highlight(app: &App, pane: Pane) -> Style {
    if app.focus == pane || (app.focus == Pane::Detail && app.detail_of == pane) {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    }
}

fn draw_session_list(frame: &mut Frame, area: Rect, app: &App, now: Timestamp) {
    let matches = app.matches(Pane::Sessions, now);
    let wide = area.width >= 64;
    let rows = app
        .order
        .iter()
        .filter_map(|id| app.store.get(id))
        .enumerate()
        .map(|(index, session)| {
            let status = app::status(session, now);
            let updated = app::latest(session).map_or("—".to_owned(), |at| format::ago(now, at));
            let mut cells = vec![Cell::from(
                app::label(session)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )];
            if wide {
                cells.push(Cell::from(
                    app::project(session).unwrap_or_else(|| "—".to_owned()),
                ));
            }
            cells.push(Cell::from(status.text).style(style_of(status.kind)));
            cells.push(Cell::from(Line::from(updated).right_aligned()));
            cells.push(Cell::from(
                Line::from(format::cost(&session.cost)).right_aligned(),
            ));
            Row::new(cells).style(row_style(&matches, index))
        });
    let mut widths = vec![Constraint::Fill(3)];
    let mut header = vec!["title"];
    if wide {
        widths.push(Constraint::Fill(1));
        header.push("repository");
    }
    widths.extend([
        Constraint::Length(14),
        Constraint::Length(4),
        Constraint::Length(9),
    ]);
    header.extend(["state", "upd", "cost"]);

    let title = format!("Sessions ({} · {} ↓)", app.filter.label(), app.sort.label());
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .row_highlight_style(highlight(app, Pane::Sessions))
        .block(pane_block(app, Pane::Sessions, title, matches.len()));
    let mut state =
        TableState::default().with_selected((!app.order.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn outcome(call: &ToolCall) -> (&'static str, Style) {
    match call.success {
        Some(true) => ("ok", Style::new().fg(Color::Green)),
        Some(false) => ("failed", Style::new().fg(Color::Red)),
        None => ("…", dim()),
    }
}

/// An outcome as one character, for a narrow column.
fn symbol(outcome: &str) -> &'static str {
    match outcome {
        "ok" => "✓",
        "failed" => "✗",
        _ => "…",
    }
}

fn draw_tool_list(frame: &mut Frame, area: Rect, app: &App, now: Timestamp) {
    let matches = app.matches(Pane::Tools, now);
    let calls = app.tools();
    let rows = calls.iter().enumerate().map(|(index, call)| {
        let (mark, style) = outcome(call);
        let name = format!(
            "{}{}",
            if call.agent_id.is_some() { "↳ " } else { "" },
            format::tool_name(&call.name)
        );
        Row::new([
            Cell::from(name),
            Cell::from(call.summary.clone().unwrap_or_default()).style(dim()),
            Cell::from(
                Line::from(call.duration_ms.map(format::duration).unwrap_or_default())
                    .right_aligned(),
            ),
            Cell::from(symbol(mark)).style(style),
        ])
        .style(row_style(&matches, index))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Fill(1),
            Constraint::Length(6),
            Constraint::Length(1),
        ],
    )
    .row_highlight_style(highlight(app, Pane::Tools))
    .block(pane_block(
        app,
        Pane::Tools,
        format!("Tool calls ({})", calls.len()),
        matches.len(),
    ));
    let mut state =
        TableState::default().with_selected((!calls.is_empty()).then_some(app.tool_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn subagent_name(subagent: &Subagent) -> &str {
    subagent
        .description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&subagent.agent_id)
}

fn draw_subagent_list(frame: &mut Frame, area: Rect, app: &App, now: Timestamp) {
    let matches = app.matches(Pane::Subagents, now);
    let subagents = app.subagents();
    let rows = subagents.iter().enumerate().map(|(index, subagent)| {
        let done = match subagent.finished {
            Some(true) => Span::styled("done", dim()),
            _ => Span::styled("…", Style::new().fg(Color::Green)),
        };
        Row::new([
            Cell::from(
                subagent
                    .agent_type
                    .clone()
                    .unwrap_or_else(|| "agent".to_owned()),
            ),
            Cell::from(subagent_name(subagent).to_owned()).style(dim()),
            Cell::from(done),
        ])
        .style(row_style(&matches, index))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Fill(1),
            Constraint::Length(4),
        ],
    )
    .row_highlight_style(highlight(app, Pane::Subagents))
    .block(pane_block(
        app,
        Pane::Subagents,
        format!("Subagents ({})", subagents.len()),
        matches.len(),
    ));
    let mut state = TableState::default()
        .with_selected((!subagents.is_empty()).then_some(app.subagent_selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let width = area.width.saturating_sub(2) as usize;
    let (title, lines) = match (app.detail_of, app.selected_session()) {
        (_, None) => (
            "Detail".to_owned(),
            vec![Line::raw(
                app.notice
                    .clone()
                    .unwrap_or_else(|| "no sessions".to_owned()),
            )],
        ),
        (Pane::Tools, Some(session)) => (
            "Tool call".to_owned(),
            match app.selected_tool() {
                Some(call) => tool_lines(session, call, now),
                None => vec![Line::styled("no tool calls", dim())],
            },
        ),
        (Pane::Subagents, Some(session)) => (
            "Subagent".to_owned(),
            match app.selected_subagent() {
                Some(subagent) => subagent_lines(session, subagent, now, width),
                None => vec![Line::styled("no subagents", dim())],
            },
        ),
        (_, Some(session)) => ("Session".to_owned(), session_lines(session, now, width)),
    };

    let rows = area.height.saturating_sub(2) as usize;
    app.viewport.detail_rows = rows;
    app.viewport.detail_lines = lines.len();
    app.detail_scroll = app.detail_scroll.min(lines.len().saturating_sub(rows));
    let title = if lines.len() > rows {
        format!(
            "{title} [{}-{}/{}]",
            app.detail_scroll + 1,
            (app.detail_scroll + rows).min(lines.len()),
            lines.len()
        )
    } else {
        title
    };
    let paragraph = Paragraph::new(lines)
        .scroll((u16::try_from(app.detail_scroll).unwrap_or(u16::MAX), 0))
        .block(pane_block(app, Pane::Detail, title, 0));
    frame.render_widget(paragraph, area);
}

fn field(name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<10}"), dim()),
        Span::raw(value),
    ])
}

fn context_line(session: &Session, width: usize) -> Line<'static> {
    let Some(context) = session.current_context() else {
        return Line::styled("context   —", dim());
    };
    let Some(fill) = context.fill() else {
        return field(
            "context",
            format!("{} of an unknown window", format::tokens(context.tokens)),
        );
    };
    let color = match fill {
        f if f >= 0.9 => Color::Red,
        f if f >= 0.7 => Color::Yellow,
        _ => Color::Green,
    };
    let label = format!(
        "{} of {} ({})  ",
        format::tokens(context.tokens),
        format::tokens(context.window.unwrap_or_default()),
        format::percent(fill)
    );
    let room = width.saturating_sub(10 + label.chars().count()).min(40);
    let filled = ((fill.clamp(0.0, 1.0) * room as f64).round() as usize).min(room);
    Line::from(vec![
        Span::styled(format!("{:<10}", "context"), dim()),
        Span::raw(label),
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled("░".repeat(room - filled), dim()),
    ])
}

fn usage_header(title: &str) -> Line<'static> {
    Line::styled(
        format!(
            "{title:<24}{:>9}{:>9}{:>9}{:>10}{:>7}",
            "in", "out", "cache", "cost", "calls"
        ),
        dim(),
    )
}

fn usage_line(name: &str, usage: &ModelUsage) -> Line<'static> {
    let cache = usage.cache_write_5m_tokens + usage.cache_write_1h_tokens + usage.cache_read_tokens;
    Line::raw(format!(
        "  {:<22}{:>9}{:>9}{:>9}{:>10}{:>7}",
        format::truncate(name, 22),
        format::tokens(usage.input_tokens),
        format::tokens(usage.output_tokens),
        format::tokens(cache),
        usage.estimated_cost().map_or("—".to_owned(), |usd| format!(
            "~{}",
            format::usd_amount(usd)
        )),
        usage.responses,
    ))
}

/// Where the session's cost came from, in words.
fn cost_note(session: &Session) -> &'static str {
    match session.cost.source {
        CostSource::Unknown => "",
        CostSource::Reported => "  reported by Claude Code",
        CostSource::ReportedPlusEstimate => "  reported, plus an estimate since",
        CostSource::Estimated => "  estimated from the price table",
    }
}

fn session_lines(session: &Session, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let status = app::status(session, now);

    lines.push(Line::styled(
        format::truncate(app::label(session), width),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<10}", "state"), dim()),
        Span::styled(status.text, style_of(status.kind)),
    ]));
    if let Some(live) = &session.live {
        if let LiveState::RunningTool(tool) = live.state() {
            if let Some(summary) = &tool.summary {
                lines.push(field(
                    "",
                    format::truncate(summary, width.saturating_sub(10)),
                ));
            }
        }
    }
    lines.push(context_line(session, width));
    if let Some(path) = session.project_path.as_ref().or(session.cwd.as_ref()) {
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
            "{}, {}, {}",
            format::count(session.turns, "turn"),
            format::count(session.tool_calls, "tool call"),
            format::count(session.subagents.len() as u64, "subagent"),
        ),
    ));
    if let Some(started) = session.started_at {
        lines.push(field(
            "started",
            format!("{} ago", format::ago(now, started)),
        ));
    }
    if let Some(prompt) = &session.last_prompt {
        lines.push(field(
            "prompt",
            format::truncate(prompt, width.saturating_sub(10)),
        ));
    }
    lines.push(field("id", session.id.clone()));

    lines.push(Line::raw(""));
    lines.push(field(
        "cost",
        format::cost(&session.cost) + cost_note(session),
    ));
    lines.push(Line::raw(""));
    lines.push(usage_header("by model"));
    for (model, usage) in &session.usage.by_model {
        lines.push(usage_line(model, usage));
    }
    if session.usage.by_model.len() != 1 {
        lines.push(usage_line("total", &session.usage.total()));
    }
    if !session.usage.by_query_source.is_empty() {
        lines.push(Line::raw(""));
        lines.push(usage_header("by source"));
        for (source, usage) in &session.usage.by_query_source {
            lines.push(usage_line(source, usage));
        }
    }

    if let Some(live) = &session.live {
        if !live.rate_limits.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("rate limits", dim()));
            for limit in &live.rate_limits {
                lines.push(Line::raw(format!(
                    "  {:<22}{:>4.0}%",
                    limit.kind, limit.percent_used
                )));
            }
        }
    }
    lines
}

fn tool_lines(session: &Session, call: &ToolCall, now: Timestamp) -> Vec<Line<'static>> {
    let (mark, style) = outcome(call);
    let mut lines = vec![
        Line::styled(call.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Line::from(vec![
            Span::styled(format!("{:<10}", "outcome"), dim()),
            Span::styled(
                if call.success.is_none() {
                    "running"
                } else {
                    mark
                },
                style,
            ),
        ]),
    ];
    if let Some(ms) = call.duration_ms {
        lines.push(field("took", format::duration(ms)));
    }
    if let Some(at) = call.started_at {
        lines.push(field("started", format!("{} ago", format::ago(now, at))));
    }
    let by = match &call.agent_id {
        None => "the main thread".to_owned(),
        Some(id) => match session.subagents.get(id) {
            Some(agent) => format!(
                "subagent {} ({})",
                agent.agent_type.as_deref().unwrap_or("agent"),
                subagent_name(agent)
            ),
            None => format!("subagent {id}"),
        },
    };
    lines.push(field("by", by));
    lines.push(field("id", call.id.clone()));
    if let Some(summary) = &call.summary {
        lines.push(Line::raw(""));
        lines.push(Line::raw(summary.clone()));
    }
    lines
}

fn subagent_lines(
    session: &Session,
    subagent: &Subagent,
    now: Timestamp,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        format::truncate(subagent_name(subagent), width),
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if let Some(kind) = &subagent.agent_type {
        lines.push(field("type", kind.clone()));
    }
    lines.push(field(
        "state",
        match subagent.finished {
            Some(true) => "done".to_owned(),
            _ => "running, or not marked done".to_owned(),
        },
    ));
    if let Some(model) = &subagent.model {
        lines.push(field("model", model.clone()));
    }
    if let Some(at) = subagent.started_at {
        lines.push(field("started", format!("{} ago", format::ago(now, at))));
    }
    if let (Some(from), Some(to)) = (subagent.started_at, subagent.last_activity) {
        let ms = (to.as_millis() - from.as_millis()).max(0) as u64;
        lines.push(field("ran for", format::duration(ms)));
    }
    lines.push(field("id", subagent.agent_id.clone()));
    lines.push(Line::raw(""));
    lines.push(usage_header("usage"));
    lines.push(usage_line(
        subagent.model.as_deref().unwrap_or("—"),
        &subagent.usage,
    ));

    let mut calls: Vec<&ToolCall> = session
        .recent_tools
        .iter()
        .filter(|call| call.agent_id.as_deref() == Some(subagent.agent_id.as_str()))
        .collect();
    calls.sort_by_key(|call| std::cmp::Reverse(call.started_at));
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "tool calls ({} in all, the latest {})",
            subagent.tool_calls,
            calls.len()
        ),
        dim(),
    ));
    let room = width.saturating_sub(2 + 15 + 1 + 7 + 2 + 6);
    for call in calls {
        let (mark, style) = outcome(call);
        lines.push(Line::from(vec![
            Span::raw(format!(
                "  {:<15}",
                format::truncate(&format::tool_name(&call.name), 14)
            )),
            Span::styled(
                format!(
                    "{:<room$}",
                    format::truncate(call.summary.as_deref().unwrap_or_default(), room)
                ),
                dim(),
            ),
            Span::raw(format!(
                " {:>7}  ",
                call.duration_ms.map(format::duration).unwrap_or_default()
            )),
            Span::styled(mark, style),
        ]));
    }
    lines
}

fn draw_usage_page(frame: &mut Frame, area: Rect, app: &App) {
    let mut by_model: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut by_source: BTreeMap<String, ModelUsage> = BTreeMap::new();
    // Repository → sessions, cost, tokens. The cost is each session's own
    // (reported where Claude Code reported it), the rest are estimates.
    let mut by_repo: BTreeMap<String, (u64, f64, u64)> = BTreeMap::new();
    let (mut total_cost, mut unknown_cost) = (0.0, 0u64);
    for session in app.store.sessions() {
        for (model, usage) in &session.usage.by_model {
            add(by_model.entry(model.clone()).or_default(), usage);
        }
        for (source, usage) in &session.usage.by_query_source {
            add(by_source.entry(source.clone()).or_default(), usage);
        }
        let repo = by_repo
            .entry(app::project(session).unwrap_or_else(|| "—".to_owned()))
            .or_default();
        repo.0 += 1;
        repo.2 += session.usage.total().total_tokens();
        match session.cost.usd {
            Some(usd) => {
                repo.1 += usd;
                total_cost += usd;
            }
            None => unknown_cost += 1,
        }
    }

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Min(20)]).areas(area);
    let [models, sources] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Min(3)]).areas(left);

    let table = |title: &str, rows: &BTreeMap<String, ModelUsage>| {
        let mut lines = vec![usage_header(title)];
        let mut sorted: Vec<_> = rows.iter().collect();
        sorted.sort_by_key(|(_, usage)| std::cmp::Reverse(usage.total_tokens()));
        let mut total = ModelUsage::default();
        for (name, usage) in sorted {
            lines.push(usage_line(name, usage));
            add(&mut total, usage);
        }
        lines.push(usage_line("total", &total));
        lines
    };
    frame.render_widget(
        Paragraph::new(table("by model", &by_model))
            .block(Block::bordered().title(" Models ").border_style(dim())),
        models,
    );
    frame.render_widget(
        Paragraph::new(table("by source", &by_source))
            .block(Block::bordered().title(" Sources ").border_style(dim())),
        sources,
    );

    let mut repos: Vec<_> = by_repo.into_iter().collect();
    repos.sort_by(|a, b| {
        b.1 .1
            .partial_cmp(&a.1 .1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut lines = vec![Line::styled(
        format!(
            "{:<24}{:>9}{:>10}{:>10}",
            "repository", "sessions", "tokens", "cost"
        ),
        dim(),
    )];
    for (name, (sessions, cost, tokens)) in &repos {
        lines.push(Line::raw(format!(
            "  {:<22}{:>9}{:>10}{:>10}",
            format::truncate(name, 22),
            sessions,
            format::tokens(*tokens),
            format::usd_amount(*cost),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(format!(
        "  {:<22}{:>9}{:>10}{:>10}",
        "total",
        app.store.len(),
        "",
        format::usd_amount(total_cost)
    )));
    if unknown_cost > 0 {
        lines.push(Line::styled(
            format!(
                "  {} without a known cost are not in the total",
                format::count(unknown_cost, "session")
            ),
            dim(),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Session costs are Claude Code's own where it reported them,",
        dim(),
    ));
    lines.push(Line::styled(
        "  estimates otherwise. Per-model and per-source costs are estimates.",
        dim(),
    ));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" Repositories ")
                .border_style(dim()),
        ),
        right,
    );
}

fn add(total: &mut ModelUsage, usage: &ModelUsage) {
    total.responses += usage.responses;
    total.input_tokens += usage.input_tokens;
    total.output_tokens += usage.output_tokens;
    total.cache_write_5m_tokens += usage.cache_write_5m_tokens;
    total.cache_write_1h_tokens += usage.cache_write_1h_tokens;
    total.cache_read_tokens += usage.cache_read_tokens;
    total.estimated_usd += usage.estimated_usd;
    total.unpriced_responses += usage.unpriced_responses;
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(typed) = &app.typing {
        Line::from(vec![
            Span::styled("/", Style::new().fg(Color::Yellow)),
            Span::raw(typed.clone()),
            Span::styled("█", dim()),
        ])
    } else if let Some(notice) = &app.notice {
        Line::styled(notice.clone(), Style::new().fg(Color::Red))
    } else if let LiveStatus::Failed(reason) = &app.live {
        Line::styled(
            format!("live events unavailable: {reason}"),
            Style::new().fg(Color::Red),
        )
    } else {
        Line::styled(
            " Tab pane  j/k move  i detail  / search  s sort  f filter  ? help  q quit",
            dim(),
        )
    };
    frame.render_widget(line, area);
}

const HELP: &[(&str, &str)] = &[
    ("1 / 2", "Sessions page / Usage page"),
    ("Tab / Shift+Tab", "next / previous pane"),
    ("h / l", "the pane to the left / right"),
    ("j / k", "down / up (scrolls the detail pane)"),
    ("gg / G", "top / bottom"),
    ("Ctrl+d / Ctrl+u", "half a page down / up"),
    ("i / Enter", "focus the detail pane"),
    ("h / Esc", "back to the list (Esc also clears a search)"),
    ("/", "search the focused list"),
    ("n / N", "next / previous match"),
    ("s", "sort sessions: updated, cost, tokens"),
    ("f", "filter sessions: all, live, open"),
    ("r", "refresh now"),
    ("?", "this help"),
    ("q / Ctrl+c", "quit"),
];

fn draw_help(frame: &mut Frame, area: Rect) {
    let width = 62.min(area.width);
    let height = (HELP.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::styled(format!(" {keys:<17}"), Style::new().fg(Color::Cyan)),
                Span::raw(*what),
            ])
        })
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys (any key closes) ")),
        popup,
    );
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
    use crate::app::tests::{fixture_app, INDEXED, WITH_SUBAGENTS};
    use crate::app::{Config, Filter, Sort};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const LATER: &str = "2026-11-03T12:00:00.000Z";

    /// Draws the app and returns the screen as text, one line per row.
    fn screen(app: &mut App, width: u16, height: u16, now: &str) -> String {
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

    fn select(app: &mut App, id: &str) {
        app.focus(Pane::Sessions);
        let at = app.order.iter().position(|other| other == id).unwrap();
        app.move_first();
        app.move_by(at as isize);
        assert_eq!(app.selected_session().unwrap().id, id);
    }

    /// The row and height of each pane's top border.
    fn pane_top(screen: &str, title: &str) -> usize {
        screen
            .lines()
            .position(|line| line.contains(title))
            .unwrap_or_else(|| panic!("no pane titled {title}\n{screen}"))
    }

    #[test]
    fn the_sessions_page_shows_three_lists_over_a_detail_pane() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        let screen = screen(&mut app, 160, 44, "2026-11-02T10:00:20.000Z");

        // Header.
        assert!(screen.contains("1:Sessions"), "{screen}");
        assert!(screen.contains("2:Usage"));
        assert!(screen.contains("3 sessions"));
        assert!(screen.contains("live off"));

        // The three lists share the top row of panes.
        let top = pane_top(&screen, "Sessions (all · updated ↓)");
        assert_eq!(pane_top(&screen, "Tool calls (5)"), top);
        assert_eq!(pane_top(&screen, "Subagents (2)"), top);
        assert!(pane_top(&screen, "┌ Session ") > top);

        // Sessions.
        assert!(screen.contains("Synthetic session title"));
        assert!(screen.contains("other.repo"));
        assert!(screen.contains("active"));

        // The selected session's tool calls, the latest first.
        let row = |needle: &str| {
            screen
                .lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no row with {needle}\n{screen}"))
        };
        assert!(row("cargo test --workspace").contains("Bash"));
        assert!(row("cargo test --workspace").contains("1.5s"));
        assert!(row("cargo test --workspace").contains('✓'));
        assert!(row("/work/example/README.md").contains('✗'));
        assert!(row("synthetic lookup").contains("example/look"));
        assert!(row("synthetic pattern").contains("↳ Grep"));

        // Its subagents.
        assert!(row("Synthetic agent task").contains("Explore"));
        assert!(row("Synthetic agent task").contains("done"));

        // The detail of the session.
        assert!(screen.contains("/work/other.repo"));
        assert!(screen.contains("claude-fable-5-2"));
        assert!(screen.contains("1 turn, 5 tool calls, 2 subagents"));
        assert!(screen.contains("context   6.7k of 1.0M (1%)"));
        assert!(screen.contains("estimated from the price table"));
        assert!(screen.contains("by model"));
        assert!(screen.contains("claude-haiku-5-5"));
        assert!(screen.contains("by source"));
        assert!(screen.contains("agent:builtin:Explore"));
        assert!(screen.contains("repl_main_thread"));
    }

    #[test]
    fn the_layout_does_not_depend_on_what_the_panes_hold() {
        let mut app = fixture_app();
        let tops = |screen: &str| {
            (
                pane_top(screen, "Sessions ("),
                pane_top(screen, "Tool calls ("),
                screen
                    .lines()
                    .position(|line| line.starts_with('┌') && !line.contains("Sessions ("))
                    .unwrap(),
            )
        };
        // A session with a lot to show, and one with almost nothing.
        select(&mut app, WITH_SUBAGENTS);
        let full = tops(&screen(&mut app, 120, 40, LATER));
        select(&mut app, INDEXED);
        let sparse = tops(&screen(&mut app, 120, 40, LATER));
        assert_eq!(full, sparse);
        // 40 rows: a header, a body of 38 split 40% / 60%, a footer.
        assert_eq!(full, (1, 1, 16));

        // Focusing a list changes what the detail pane describes, not where
        // it is.
        select(&mut app, WITH_SUBAGENTS);
        app.focus(Pane::Tools);
        let on_tools = screen(&mut app, 120, 40, LATER);
        assert_eq!(pane_top(&on_tools, "┌ Tool call "), 16);
        app.focus(Pane::Subagents);
        let on_subagents = screen(&mut app, 120, 40, LATER);
        assert_eq!(pane_top(&on_subagents, "┌ Subagent "), 16);
    }

    #[test]
    fn the_detail_pane_follows_the_focused_list() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);

        app.focus(Pane::Tools);
        app.move_last(); // the oldest call: Bash
        let tool = screen(&mut app, 140, 44, LATER);
        let detail = &tool[tool.find("┌ Tool call ").unwrap()..];
        assert!(detail.contains("Bash"), "{tool}");
        assert!(detail.contains("outcome   ok"));
        assert!(detail.contains("took      1.5s"));
        assert!(detail.contains("by        the main thread"));
        assert!(detail.contains("toolu_01"));
        assert!(detail.contains("cargo test --workspace"));

        app.move_first(); // the latest: a subagent's Grep
        let tool = screen(&mut app, 140, 44, LATER);
        let detail = &tool[tool.find("┌ Tool call ").unwrap()..];
        assert!(detail.contains("by        subagent"), "{tool}");

        app.focus(Pane::Subagents);
        let agent = screen(&mut app, 140, 44, LATER);
        let detail = &agent[agent.find("┌ Subagent ").unwrap()..];
        assert!(detail.contains("type      Explore") || detail.contains("id        b2c3"));
        assert!(detail.contains("claude-haiku-5-5"));
        assert!(detail.contains("tool calls (1 in all, the latest 1)"));
        assert!(detail.contains("Grep"));
        assert!(detail.contains("synthetic pattern"));

        // Into the detail pane and back: it keeps describing the same list.
        app.focus(Pane::Detail);
        assert!(screen(&mut app, 140, 44, LATER).contains("┌ Subagent "));
        app.back();
        assert_eq!(app.focus, Pane::Subagents);
    }

    #[test]
    fn the_detail_pane_scrolls_and_stops_at_its_end() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        // Short: the session's detail does not fit.
        let first = screen(&mut app, 120, 20, LATER);
        assert!(first.contains("Session [1-"), "{first}");
        assert!(!first.contains("by source"));

        app.focus(Pane::Detail);
        app.move_last();
        let last = screen(&mut app, 120, 20, LATER);
        assert!(last.contains("repl_main_thread"), "{last}");
        let total = app.viewport.detail_lines;
        assert!(last.contains(&format!("-{total}/{total}]")));
        // Past the end stays at the end.
        let at = app.detail_scroll;
        app.move_by(50);
        screen(&mut app, 120, 20, LATER);
        assert_eq!(app.detail_scroll, at);

        app.move_by(-app.half_page());
        screen(&mut app, 120, 20, LATER);
        assert!(app.detail_scroll < at);
        app.move_first();
        assert_eq!(app.detail_scroll, 0);
    }

    #[test]
    fn a_search_marks_its_matches_in_the_pane_title_and_moves_between_them() {
        let mut app = fixture_app();
        app.focus(Pane::Sessions);
        app.start_search();
        app.typing = Some("SYNTHETIC".to_owned());
        let typing = screen(&mut app, 140, 30, LATER);
        assert!(typing.lines().last().unwrap().starts_with("/SYNTHETIC"));

        app.submit_search();
        let found = screen(&mut app, 140, 30, LATER);
        // Two of the three sessions have a title; case does not matter.
        assert!(found.contains("/synthetic (2)"), "{found}");
        let first = app.selected;
        app.next_match(1);
        assert_ne!(app.selected, first);
        app.next_match(1);
        assert_eq!(app.selected, first, "the matches wrap around");
        app.next_match(-1);
        assert_ne!(app.selected, first);

        // A search belongs to its pane.
        select(&mut app, WITH_SUBAGENTS);
        app.focus(Pane::Tools);
        app.start_search();
        app.typing = Some("cargo".to_owned());
        app.submit_search();
        let tools = screen(&mut app, 140, 30, LATER);
        assert!(tools.contains("Tool calls (5) /cargo (1)"), "{tools}");
        assert!(!tools.contains("/synthetic"));
        assert_eq!(app.selected_tool().unwrap().name, "Bash");

        app.back();
        assert_eq!(app.search, None);
    }

    #[test]
    fn sort_and_filter_show_in_the_title_and_change_the_list() {
        let mut app = fixture_app();
        app.cycle_sort();
        assert_eq!(app.sort, Sort::Cost);
        let by_cost = screen(&mut app, 140, 30, LATER);
        assert!(by_cost.contains("Sessions (all · cost ↓)"), "{by_cost}");
        // The most expensive first; the one without a cost last.
        assert_eq!(app.order[0], WITH_SUBAGENTS);
        assert_eq!(app.order[1], INDEXED);

        app.cycle_sort();
        assert_eq!(app.sort, Sort::Tokens);
        assert_eq!(app.order[0], WITH_SUBAGENTS);

        // Nothing in the fixtures is running: the live filter empties the
        // list, and the panes say so instead of failing.
        app.cycle_filter();
        assert_eq!(app.filter, Filter::Live);
        let live = screen(&mut app, 140, 30, LATER);
        assert!(live.contains("Sessions (live · tokens ↓)"), "{live}");
        assert!(app.order.is_empty());
        assert!(live.contains("Tool calls (0)"));
        assert!(live.contains("no sessions"));

        app.cycle_filter();
        assert_eq!(app.filter, Filter::Open);
        assert_eq!(app.order.len(), 3);
    }

    #[test]
    fn the_usage_page_totals_every_session() {
        let mut app = fixture_app();
        app.page = Page::Usage;
        let screen = screen(&mut app, 160, 40, LATER);
        assert!(screen.contains("Models"), "{screen}");
        assert!(screen.contains("claude-fable-5-2"));
        assert!(screen.contains("claude-haiku-5-5"));
        assert!(screen.contains("Sources"));
        assert!(screen.contains("repl_main_thread"));
        assert!(screen.contains("agent:builtin:Explore"));
        assert!(screen.contains("Repositories"));
        assert!(screen.contains("other.repo"));
        assert!(screen.contains("example"));
        // $0.019256 + 2 × $0.0002388 + $0.008896, to the cent.
        assert!(screen.contains("$0.03"));
        assert!(screen.contains("1 session without a known cost"));
    }

    #[test]
    fn the_help_lists_the_keys_over_the_page() {
        let mut app = fixture_app();
        app.help = true;
        let screen = screen(&mut app, 120, 40, LATER);
        assert!(screen.contains("Keys (any key closes)"), "{screen}");
        assert!(screen.contains("Ctrl+d / Ctrl+u"));
        assert!(screen.contains("filter sessions"));
    }

    #[test]
    fn nothing_panics_at_small_sizes() {
        let mut app = fixture_app();
        app.help = true;
        for page in [Page::Sessions, Page::Usage] {
            app.page = page;
            for (width, height) in [(1, 1), (10, 3), (30, 5), (80, 10), (200, 2), (60, 24)] {
                screen(&mut app, width, height, LATER);
            }
        }
    }

    #[test]
    fn an_empty_store_shows_the_notice() {
        let mut app = App::new(Config {
            projects_dir: crate::app::tests::fixtures().join("no-such-directory"),
            live: false,
            socket_dir: None,
        });
        let screen = screen(&mut app, 120, 20, LATER);
        assert!(screen.contains("0 sessions"), "{screen}");
        assert!(screen.contains("no projects directory"));
    }

    #[test]
    #[ignore = "prints the screen for a human to look at"]
    fn show_the_screen() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        println!("{}", screen(&mut app, 150, 40, "2026-11-02T10:00:20.000Z"));
        app.focus(Pane::Tools);
        println!("{}", screen(&mut app, 100, 30, "2026-11-02T10:00:20.000Z"));
        app.page = Page::Usage;
        println!("{}", screen(&mut app, 150, 30, "2026-11-02T10:00:20.000Z"));
    }

    #[test]
    fn no_unmodelled_text_reaches_the_screen() {
        let mut app = fixture_app();
        for pane in [Pane::Sessions, Pane::Tools, Pane::Subagents] {
            for id in [INDEXED, WITH_SUBAGENTS] {
                select(&mut app, id);
                app.focus(pane);
                let screen = screen(&mut app, 160, 50, LATER);
                assert!(!screen.contains("SYNTHETIC-TEXT"), "{screen}");
            }
        }
        app.page = Page::Usage;
        assert!(!screen(&mut app, 160, 50, LATER).contains("SYNTHETIC-TEXT"));
    }
}

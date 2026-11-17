//! Drawing. Each list page is a list over a detail pane in fixed proportions
//! (the shape of vig's pages); the Usage page is three tables.

use std::collections::BTreeMap;

use iktinos::record::Timestamp;
use iktinos::session::{CostSource, LiveState, ModelUsage, Session, Subagent, ToolCall};
use iktinos::trace::{between, Item, Request, SpanKind, Turn};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::{self, App, LiveStatus, Page, StatusKind, TraceItem, TraceRow};
use crate::format;
use crate::highlight;

pub fn draw(frame: &mut Frame, app: &mut App, now: Timestamp) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app, now);
    if app.page.has_list() {
        // Fixed proportions, whatever the panes hold.
        let [list, detail] =
            Layout::vertical([Constraint::Percentage(45), Constraint::Min(3)]).areas(body);
        app.viewport.list_rows = (list.height as usize).saturating_sub(3);
        match app.page {
            Page::Sessions => draw_session_list(frame, list, app, now),
            Page::Tools => draw_tool_list(frame, list, app, now),
            Page::Trace => draw_trace_list(frame, list, app, now),
            _ => draw_subagent_list(frame, list, app, now),
        }
        if app.page == Page::Subagents {
            // The subagent, and beside it the tool call chosen in it. The
            // subagent's side is as wide as its tables, where there is room.
            let left = 72.min(detail.width * 11 / 20);
            let [subagent, call] =
                Layout::horizontal([Constraint::Length(left), Constraint::Min(0)]).areas(detail);
            draw_detail(frame, subagent, app, now);
            draw_call(frame, call, app, now);
        } else {
            draw_detail(frame, detail, app, now);
        }
    } else {
        draw_usage_page(frame, body, app);
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
    let mut spans = Vec::new();
    for (index, page) in Page::ALL.into_iter().enumerate() {
        let style = if app.page == page {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            dim()
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {}:{} ", index + 1, page.title()),
            style,
        ));
    }
    spans.push(Span::raw(format!(
        "   {} sessions  {} busy",
        app.store.len(),
        counts[0]
    )));
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

/// A pane's frame: its title and whether it has the focus.
fn pane_block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        dim()
    };
    Block::bordered()
        .title(format!(" {title} "))
        .border_style(style)
}

/// The title of a list pane: what it lists, the session it is narrowed to
/// and the search in force.
fn list_title(app: &App, what: String, matches: usize) -> String {
    let mut title = what;
    if app.page != Page::Sessions {
        if let Some(session) = app.scoped_session() {
            title.push_str(&format!(
                " of {}",
                format::truncate(app::label(session), 40)
            ));
        }
        if let (Page::Tools, Some(agent)) = (app.page, app.scoped_subagent()) {
            title.push_str(&format!(
                " › {}: {}",
                agent.agent_type.as_deref().unwrap_or("agent"),
                format::truncate(subagent_name(agent), 40)
            ));
        }
    }
    if let Some(search) = app.search.as_ref().filter(|s| s.page == app.page) {
        title.push_str(&format!("  /{} ({matches})", search.query));
    }
    title
}

/// The style of a row the search matches.
fn row_style(matches: &[usize], index: usize) -> Style {
    if matches.contains(&index) {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new()
    }
}

fn session_cell(session: &Session) -> Cell<'static> {
    Cell::from(
        app::label(session)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// Draws a list page's table, keeping its scroll position between draws: the
/// list moves only when the cursor would leave it.
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    index: usize,
    table: Table<'static>,
    selected: Option<usize>,
) {
    let table = table
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2);
    let mut state = TableState::default()
        .with_offset(app.offsets[index])
        .with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
    app.offsets[index] = state.offset();
}

fn draw_session_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let matches = app.matches(Page::Sessions, now);
    let rows: Vec<Row<'static>> = app
        .order
        .iter()
        .filter_map(|id| app.store.get(id))
        .enumerate()
        .map(|(index, session)| {
            let status = app::status(session, now);
            let updated = app::latest(session).map_or("—".to_owned(), |at| format::ago(now, at));
            // The session the other pages are narrowed to.
            let scoped = app.scope.as_deref() == Some(session.id.as_str());
            Row::new([
                Cell::from(if scoped { "●" } else { "" }).style(Style::new().fg(Color::Cyan)),
                session_cell(session),
                Cell::from(app::project(session).unwrap_or_else(|| "—".to_owned())),
                Cell::from(session.git_branch.clone().unwrap_or_else(|| "—".to_owned())),
                Cell::from(status.text).style(style_of(status.kind)),
                Cell::from(Line::from(updated).right_aligned()),
                Cell::from(
                    Line::from(format::tokens(session.usage.total().total_tokens()))
                        .right_aligned(),
                ),
                Cell::from(Line::from(format::cost(&session.cost)).right_aligned()),
            ])
            .style(row_style(&matches, index))
        })
        .collect();
    let title = list_title(
        app,
        format!("Sessions ({} · {} ↓)", app.filter.label(), app.sort.label()),
        matches.len(),
    );
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(5),
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Length(15),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(9),
        ],
    )
    .header(
        Row::new([
            "",
            "title",
            "repository",
            "branch",
            "state",
            "updated",
            "tokens",
            "cost",
        ])
        .style(dim()),
    )
    .block(pane_block(title, !app.in_detail));
    let selected = (!app.order.is_empty()).then_some(app.selected);
    draw_list(frame, area, app, 0, table, selected);
}

fn outcome(call: &ToolCall) -> (&'static str, Style) {
    match call.success {
        Some(true) => ("ok", Style::new().fg(Color::Green)),
        Some(false) => ("failed", Style::new().fg(Color::Red)),
        None => ("running", dim()),
    }
}

fn draw_tool_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let matches = app.matches(Page::Tools, now);
    // The session column says nothing once the page is narrowed to one.
    let scoped = app.scope.is_some();
    let calls = app.tools();
    let count = calls.len();
    let rows: Vec<Row<'static>> = calls
        .iter()
        .enumerate()
        .map(|(index, (session, call))| {
            let (mark, style) = outcome(call);
            let name = format!(
                "{}{}",
                if call.agent_id.is_some() { "↳ " } else { "" },
                format::tool_name(&call.name)
            );
            let when = call
                .started_at
                .map_or("—".to_owned(), |at| format::ago(now, at));
            let mut cells = vec![Cell::from(Line::from(when).right_aligned())];
            if !scoped {
                cells.push(session_cell(session));
            }
            cells.extend([
                Cell::from(name),
                Cell::from(call.summary.clone().unwrap_or_default()),
                Cell::from(
                    Line::from(call.duration_ms.map(format::duration).unwrap_or_default())
                        .right_aligned(),
                ),
                Cell::from(mark).style(style),
            ]);
            Row::new(cells).style(row_style(&matches, index))
        })
        .collect();
    // What the call was about gets every column the others can spare.
    let mut widths = vec![Constraint::Length(4)];
    let mut header = vec!["when"];
    if !scoped {
        widths.push(Constraint::Max(22));
        header.push("session");
    }
    widths.extend([
        Constraint::Max(18),
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(7),
    ]);
    header.extend(["tool", "what", "took", ""]);
    let title = list_title(app, format!("Tool calls ({count})"), matches.len());
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .block(pane_block(title, !app.in_detail));
    let selected = (count > 0).then_some(app.tool_selected);
    draw_list(frame, area, app, 1, table, selected);
}

fn subagent_name(subagent: &Subagent) -> &str {
    subagent
        .description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&subagent.agent_id)
}

fn draw_subagent_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let matches = app.matches(Page::Subagents, now);
    let scoped = app.scope.is_some();
    let subagents = app.subagents();
    let count = subagents.len();
    let rows: Vec<Row<'static>> = subagents
        .iter()
        .enumerate()
        .map(|(index, (session, subagent))| {
            let done = match subagent.finished {
                Some(true) => Span::styled("done", dim()),
                _ => Span::styled("running", Style::new().fg(Color::Green)),
            };
            let when = subagent
                .last_activity
                .map_or("—".to_owned(), |at| format::ago(now, at));
            let mut cells = vec![Cell::from(Line::from(when).right_aligned())];
            if !scoped {
                cells.push(session_cell(session));
            }
            let entered = app.agent_scope.as_deref() == Some(subagent.agent_id.as_str())
                && app.scope.as_deref() == Some(session.id.as_str());
            cells.extend([
                Cell::from(format!(
                    "{}{}",
                    if entered { "● " } else { "" },
                    subagent.agent_type.as_deref().unwrap_or("agent")
                )),
                Cell::from(subagent_name(subagent).to_owned()),
                Cell::from(
                    subagent
                        .model
                        .as_deref()
                        .map_or("—".to_owned(), format::model_name),
                )
                .style(dim()),
                Cell::from(
                    Line::from(format::tokens(subagent.usage.total_tokens())).right_aligned(),
                ),
                Cell::from(
                    Line::from(
                        subagent
                            .usage
                            .estimated_cost()
                            .map_or("—".to_owned(), |usd| {
                                format!("~{}", format::usd_amount(usd))
                            }),
                    )
                    .right_aligned(),
                ),
                Cell::from(done),
            ]);
            Row::new(cells).style(row_style(&matches, index))
        })
        .collect();
    // The task gets every column the others can spare.
    let mut widths = vec![Constraint::Length(4)];
    let mut header = vec!["when"];
    if !scoped {
        widths.push(Constraint::Max(22));
        header.push("session");
    }
    widths.extend([
        Constraint::Max(15),
        Constraint::Fill(1),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(7),
    ]);
    header.extend(["type", "task", "model", "tokens", "cost", ""]);
    let title = list_title(app, format!("Subagents ({count})"), matches.len());
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .block(pane_block(title, !app.in_detail));
    let selected = (count > 0).then_some(app.subagent_selected);
    draw_list(frame, area, app, 2, table, selected);
}

/// Whether a turn is still going: it has no end, it is its session's latest
/// and the session is at work. The transcript alone cannot tell a turn that
/// runs from one that was cut off.
fn turn_runs(session: &Session, turn: &Turn, now: Timestamp) -> bool {
    turn.ended_at.is_none()
        && session
            .trace
            .back()
            .is_some_and(|last| std::ptr::eq(last, turn))
        && app::status(session, now).kind == StatusKind::Busy
}

/// A span as a bar on a time axis from `from` to `to`, `width` columns wide.
/// One without an end is a single column.
fn bar(
    start: Option<Timestamp>,
    end: Option<Timestamp>,
    (from, to): (Timestamp, Timestamp),
    width: usize,
) -> String {
    let (Some(start), true) = (start, width > 0) else {
        return String::new();
    };
    let total = (to.as_millis() - from.as_millis()).max(1) as f64;
    let column = |at: Timestamp| {
        ((at.as_millis() - from.as_millis()) as f64 / total).clamp(0.0, 1.0) * width as f64
    };
    let first = (column(start).floor() as usize).min(width - 1);
    let last = end
        .map_or(first + 1, |end| column(end).ceil() as usize)
        .clamp(first + 1, width);
    format!("{}{}", " ".repeat(first), "█".repeat(last - first))
}

/// What a row of the Trace page is called, and what is said beside it.
fn trace_label(row: &TraceRow) -> (String, Style, String) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    match row.item {
        TraceItem::Turn => {
            let spans = &row.turn.thread.spans;
            let calls = spans
                .iter()
                .filter(|span| matches!(span.kind, SpanKind::Tool(_)))
                .count();
            (
                "turn".to_owned(),
                bold,
                format!(
                    "{}, {}",
                    format::count((spans.len() - calls) as u64, "request"),
                    format::count(calls as u64, "tool call")
                ),
            )
        }
        TraceItem::Span(span) => match &span.kind {
            SpanKind::Tool(call) => (
                format::tool_name(&call.name),
                Style::new(),
                call.summary.clone().unwrap_or_default(),
            ),
            SpanKind::Request(request) => (
                "request".to_owned(),
                Style::new().fg(Color::Blue),
                request
                    .model
                    .as_deref()
                    .map(format::model_name)
                    .unwrap_or_default(),
            ),
            _ => (String::new(), Style::new(), String::new()),
        },
        TraceItem::Subagent(subagent) => (
            format!("↳ {}", subagent.agent_type.as_deref().unwrap_or("agent")),
            Style::new().fg(Color::Magenta),
            subagent_name(subagent).to_owned(),
        ),
    }
}

fn draw_trace_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let matches = app.matches(Page::Trace, now);
    let scoped = app.scope.is_some();
    let inner = area.width.saturating_sub(2) as usize;
    // The time axis takes a third of the pane, where there is room for one.
    let axis = if inner >= 70 { (inner / 3).min(60) } else { 0 };
    let listed = app.trace_rows();
    let count = listed.len();
    let turns = listed
        .iter()
        .filter(|row| matches!(row.item, TraceItem::Turn))
        .count();
    let rows: Vec<Row<'static>> = listed
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let is_turn = matches!(row.item, TraceItem::Turn);
            let runs = turn_runs(row.session, row.turn, now);
            // The axis: the turn from its start to its end, or to now.
            let window = row
                .turn
                .started_at
                .zip(if runs { Some(now) } else { row.turn_end });
            let (start, end) = match row.item {
                TraceItem::Turn => (row.turn.started_at, window.map(|(_, to)| to)),
                TraceItem::Span(span) => (span.started_at, span.ended_at),
                TraceItem::Subagent(subagent) => (subagent.started_at, subagent.last_activity),
            };
            // What has not ended reaches to now while the turn runs.
            let end = end.or(runs.then_some(now));
            let took = match row.item {
                TraceItem::Turn => row.turn.duration_ms.or_else(|| between(start, end)),
                _ => between(start, end),
            };
            let (mark, mark_style, colour) = match row.item {
                TraceItem::Turn if row.turn.ended_at.is_some() => ("done", dim(), Color::DarkGray),
                TraceItem::Turn if runs => {
                    ("running", Style::new().fg(Color::Green), Color::DarkGray)
                }
                TraceItem::Turn => ("no end", dim(), Color::DarkGray),
                TraceItem::Span(span) => match &span.kind {
                    SpanKind::Tool(call) => {
                        let (mark, style) = outcome(call);
                        let colour = if call.success == Some(false) {
                            Color::Red
                        } else {
                            Color::Green
                        };
                        (mark, style, colour)
                    }
                    _ => ("", dim(), Color::Blue),
                },
                TraceItem::Subagent(subagent) => match subagent.finished {
                    Some(true) => ("done", dim(), Color::Magenta),
                    _ => ("running", Style::new().fg(Color::Green), Color::Magenta),
                },
            };
            let stop = match row.item {
                TraceItem::Span(span) => match &span.kind {
                    SpanKind::Request(request) => request.stop_reason.clone(),
                    _ => None,
                },
                _ => None,
            };

            let (name, style, beside) = trace_label(row);
            let fold = match row.open {
                Some(true) => "▾ ",
                Some(false) => "▸ ",
                None => "  ",
            };
            let label = Line::from(vec![
                Span::raw(format!("{}{fold}", "  ".repeat(row.depth))),
                Span::styled(name, style),
                Span::styled(format!("  {beside}"), dim()),
            ]);
            let when = row
                .turn
                .started_at
                .filter(|_| is_turn)
                .map_or(String::new(), |at| format::ago(now, at));
            let mut cells = vec![Cell::from(Line::from(when).right_aligned())];
            if !scoped {
                cells.push(if is_turn {
                    session_cell(row.session)
                } else {
                    Cell::from("")
                });
            }
            cells.extend([
                Cell::from(label),
                Cell::from(
                    Line::from(took.map(format::duration).unwrap_or_default()).right_aligned(),
                ),
                match stop {
                    Some(stop) => Cell::from(stop).style(dim()),
                    None => Cell::from(mark).style(mark_style),
                },
            ]);
            if axis > 0 {
                let drawn = match (window, is_turn) {
                    // A turn is the whole axis: a rule for what is under it.
                    (Some(_), true) if row.open == Some(true) => "─".repeat(axis),
                    (Some(_), true) => String::new(),
                    (Some(window), false) => bar(start, end, window, axis),
                    (None, _) => String::new(),
                };
                cells.push(Cell::from(drawn).style(Style::new().fg(colour)));
            }
            Row::new(cells).style(row_style(&matches, index))
        })
        .collect();

    let mut widths = vec![Constraint::Length(4)];
    let mut header = vec!["when"];
    if !scoped {
        widths.push(Constraint::Max(22));
        header.push("session");
    }
    widths.extend([
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(8),
    ]);
    header.extend(["span", "took", ""]);
    if axis > 0 {
        widths.push(Constraint::Length(axis as u16));
        header.push("time");
    }
    let title = list_title(
        app,
        format!("Trace ({})", format::count(turns as u64, "turn")),
        matches.len(),
    );
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .block(pane_block(title, !app.in_detail));
    let selected = (count > 0).then_some(app.trace_selected);
    draw_list(frame, area, app, Page::Trace.list_index(), table, selected);
}

/// The detail of a row of the Trace page.
fn trace_lines(row: &TraceRow, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    match row.item {
        TraceItem::Turn => turn_lines(row, now),
        TraceItem::Span(span) => match &span.kind {
            SpanKind::Tool(call) => tool_lines(row.session, call, now, width),
            SpanKind::Request(request) => {
                request_lines(request, span.started_at, span.duration_ms(), now)
            }
            _ => Vec::new(),
        },
        TraceItem::Subagent(subagent) => {
            subagent_lines(row.session, subagent, now, width, 0, false).0
        }
    }
}

/// A turn: how long it took and where the time went.
fn turn_lines(row: &TraceRow, now: Timestamp) -> Vec<Line<'static>> {
    let (session, turn) = (row.session, row.turn);
    let runs = turn_runs(session, turn, now);
    let end = if runs { Some(now) } else { row.turn_end };
    let mut lines = vec![
        Line::styled("Turn", Style::new().add_modifier(Modifier::BOLD)),
        field(
            "session",
            app::label(session)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
        ),
        field(
            "state",
            match (turn.ended_at.is_some(), runs) {
                (true, _) => "done",
                (false, true) => "running",
                (false, false) => "no end recorded: cut off, or not read to its end",
            }
            .to_owned(),
        ),
    ];
    if let Some(at) = turn.started_at {
        lines.push(field("started", format!("{} ago", format::ago(now, at))));
    }
    let took = turn.duration_ms.or_else(|| between(turn.started_at, end));
    if let Some(ms) = took {
        let note = if turn.duration_ms.is_some() {
            ""
        } else {
            " so far, by the records read"
        };
        lines.push(field("took", format!("{}{note}", format::duration(ms))));
    }

    // Where the time went, on the main thread; a subagent's time is inside
    // the call that spawned it.
    let (mut requests, mut request_ms, mut calls, mut call_ms) = (0u64, 0u64, 0u64, 0u64);
    for span in &turn.thread.spans {
        let ms = span.duration_ms().unwrap_or(0);
        match span.kind {
            SpanKind::Tool(_) => {
                calls += 1;
                call_ms += ms;
            }
            _ => {
                requests += 1;
                request_ms += ms;
            }
        }
    }
    lines.push(Line::raw(""));
    lines.push(field(
        "requests",
        format!(
            "{requests}, {} in all (approximate)",
            format::duration(request_ms)
        ),
    ));
    lines.push(field(
        "tools",
        format!("{calls}, {} in all", format::duration(call_ms)),
    ));
    if let Some(ms) = took {
        let rest = ms.saturating_sub(request_ms).saturating_sub(call_ms);
        lines.push(field("the rest", format::duration(rest)));
    }

    // Tokens and cost, subagents included.
    let (mut subagents, mut tokens, mut usd, mut unpriced) = (0u64, 0u64, 0.0, false);
    for node in session.walk(turn) {
        match node.item {
            Item::Subagent(_) => subagents += 1,
            Item::Span(span) => {
                if let SpanKind::Request(request) = &span.kind {
                    if let Some(used) = request.tokens {
                        tokens += used.input
                            + used.output
                            + used.cache_write_5m
                            + used.cache_write_1h
                            + used.cache_read;
                        match request.usd {
                            Some(cost) => usd += cost,
                            None => unpriced = true,
                        }
                    }
                }
            }
        }
    }
    lines.push(Line::raw(""));
    lines.push(field("subagents", subagents.to_string()));
    lines.push(field("tokens", format::tokens(tokens)));
    lines.push(field(
        "cost",
        format!(
            "~{}{}",
            format::usd_amount(usd),
            if unpriced {
                "  without the requests of an unknown model"
            } else {
                ""
            }
        ),
    ));
    if turn.thread.dropped > 0 {
        lines.push(field(
            "",
            format!("{} more spans were not kept", turn.thread.dropped),
        ));
    }
    if !turn.id.is_empty() {
        lines.push(field("id", turn.id.clone()));
    }
    lines
}

/// One API response.
fn request_lines(
    request: &Request,
    started_at: Option<Timestamp>,
    took: Option<u64>,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        "API request",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    if let Some(model) = &request.model {
        lines.push(field("model", model.clone()));
    }
    if let Some(source) = &request.query_source {
        lines.push(field("source", source.clone()));
    }
    if let Some(stop) = &request.stop_reason {
        lines.push(field("stop", stop.clone()));
    }
    if let Some(at) = started_at {
        lines.push(field("started", format!("{} ago", format::ago(now, at))));
    }
    if let Some(ms) = took {
        lines.push(field(
            "took",
            format!(
                "{}  approximate: from the request to its last block",
                format::duration(ms)
            ),
        ));
    }
    if let Some(tokens) = request.tokens {
        lines.push(Line::raw(""));
        lines.push(field("input", format::tokens(tokens.input)));
        lines.push(field("output", format::tokens(tokens.output)));
        lines.push(field(
            "cache",
            format!(
                "{} read, {} written",
                format::tokens(tokens.cache_read),
                format::tokens(tokens.cache_write_5m + tokens.cache_write_1h)
            ),
        ));
        lines.push(field(
            "cost",
            request.usd.map_or("unknown model".to_owned(), |usd| {
                format!("~{}", format::usd_amount(usd))
            }),
        ));
    }
    if let Some(id) = &request.message_id {
        lines.push(field("id", id.clone()));
    }
    lines
}

/// `text` broken into lines of at most `width` columns, at spaces where
/// there are any.
fn wrap(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut used = 0;
        for word in paragraph.split_inclusive(' ') {
            let mut word = word;
            // A word wider than a line is cut wherever it has to be.
            while Span::raw(word).width() > width.saturating_sub(used) {
                if used > 0 {
                    lines.push(Line::raw(std::mem::take(&mut line)));
                    used = 0;
                    if Span::raw(word).width() <= width {
                        break;
                    }
                }
                let mut cut = 0;
                let mut columns = 0;
                for (at, c) in word.char_indices() {
                    let w = Span::raw(c.to_string()).width();
                    if columns + w > width {
                        break;
                    }
                    columns += w;
                    cut = at + c.len_utf8();
                }
                if cut == 0 {
                    break;
                }
                lines.push(Line::raw(word[..cut].to_owned()));
                word = &word[cut..];
            }
            line.push_str(word);
            used += Span::raw(word).width();
        }
        lines.push(Line::raw(line));
    }
    lines
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let width = area.width.saturating_sub(2) as usize;
    let empty = |text: &str| vec![Line::styled(text.to_owned(), dim())];
    // The line of the tool call chosen in a subagent's detail, and whether
    // it is the first: the pane scrolls to keep it in view.
    let mut cursor = None;
    let (title, lines) = match app.page {
        Page::Tools => (
            "Tool call",
            match app.selected_tool() {
                Some((session, call)) => tool_lines(session, call, now, width),
                None => empty("no tool calls"),
            },
        ),
        Page::Trace => (
            "Span",
            match app.selected_row() {
                Some(row) => trace_lines(&row, now, width),
                None => empty("no turns"),
            },
        ),
        Page::Subagents => (
            "Subagent",
            match app.selected_subagent() {
                Some((session, subagent)) => {
                    let (lines, at) = subagent_lines(
                        session,
                        subagent,
                        now,
                        width,
                        app.call_selected,
                        app.choosing_call(),
                    );
                    // With the focus on the list above, the pane stays at
                    // its top.
                    cursor = at.filter(|_| app.in_detail);
                    lines
                }
                None => empty("no subagents"),
            },
        ),
        _ => (
            "Session",
            match app.selected_session() {
                Some(session) => session_lines(session, now, width),
                None => vec![Line::raw(
                    app.notice
                        .clone()
                        .unwrap_or_else(|| "no sessions".to_owned()),
                )],
            },
        ),
    };

    let rows = area.height.saturating_sub(2) as usize;
    app.viewport.detail_rows = rows;
    app.viewport.detail_lines = lines.len();
    if let Some((line, first)) = cursor {
        if first || line >= app.detail_scroll + rows {
            // The first call shows as much of what is above it as fits.
            app.detail_scroll = (line + 1).saturating_sub(rows);
        } else if line < app.detail_scroll {
            app.detail_scroll = line;
        }
    }
    app.detail_scroll = app.detail_scroll.min(lines.len().saturating_sub(rows));
    let title = if lines.len() > rows {
        format!(
            "{title} [{}-{}/{}]",
            app.detail_scroll + 1,
            (app.detail_scroll + rows).min(lines.len()),
            lines.len()
        )
    } else {
        title.to_owned()
    };
    let paragraph = Paragraph::new(lines)
        .scroll((u16::try_from(app.detail_scroll).unwrap_or(u16::MAX), 0))
        .block(pane_block(title, app.in_detail && !app.in_call()));
    frame.render_widget(paragraph, area);
}

/// The pane beside a subagent's detail: the tool call chosen there, with
/// the whole of its command.
fn draw_call(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let width = area.width.saturating_sub(2) as usize;
    let lines = match app.selected_call() {
        Some(call) => call_lines(call, now, width),
        None => vec![Line::styled("no tool calls", dim())],
    };
    let rows = area.height.saturating_sub(2) as usize;
    app.viewport.call_rows = rows;
    app.viewport.call_lines = lines.len();
    app.call_scroll = app.call_scroll.min(lines.len().saturating_sub(rows));
    let title = if lines.len() > rows {
        format!(
            "Tool call [{}-{}/{}]",
            app.call_scroll + 1,
            (app.call_scroll + rows).min(lines.len()),
            lines.len()
        )
    } else {
        "Tool call".to_owned()
    };
    let paragraph = Paragraph::new(lines)
        .scroll((u16::try_from(app.call_scroll).unwrap_or(u16::MAX), 0))
        .block(pane_block(title, app.in_call()));
    frame.render_widget(paragraph, area);
}

/// What a tool call was about, in full: every line of a command, wrapped,
/// and a shell command in colour.
fn command_lines(call: &ToolCall, width: usize) -> Vec<Line<'static>> {
    let Some(text) = call.input.as_ref().or(call.summary.as_ref()) else {
        return Vec::new();
    };
    if call.name != "Bash" {
        return wrap(text, width);
    }
    let mut lines = Vec::new();
    for (paragraph, styles) in text.lines().zip(highlight::bash(text)) {
        // The wrapped lines are the paragraph's characters in order, so the
        // styles follow by position.
        let mut styles = styles.into_iter();
        for line in wrap(paragraph, width) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut last = None;
            for c in line.spans.iter().flat_map(|span| span.content.chars()) {
                let style = styles.next().unwrap_or_default();
                match spans.last_mut() {
                    Some(span) if last == Some(style) => span.content.to_mut().push(c),
                    _ => spans.push(Span::styled(c.to_string(), style)),
                }
                last = Some(style);
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// A tool call in the pane beside its subagent: the command first.
fn call_lines(call: &ToolCall, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    let (mark, style) = outcome(call);
    let mut head = vec![
        Span::styled(call.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(mark, style),
    ];
    if let Some(ms) = call.duration_ms {
        head.push(Span::styled(
            format!("  took {}", format::duration(ms)),
            dim(),
        ));
    }
    if let Some(at) = call.started_at {
        head.push(Span::styled(
            format!("  {} ago", format::ago(now, at)),
            dim(),
        ));
    }
    let mut lines = vec![Line::from(head)];
    let command = command_lines(call, width);
    if !command.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(command);
    }
    lines.push(Line::raw(""));
    lines.push(field("id", call.id.clone()));
    lines
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

    lines.extend(
        wrap(app::label(session), width)
            .into_iter()
            .map(|line| line.style(Style::new().add_modifier(Modifier::BOLD))),
    );
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

fn tool_lines(
    session: &Session,
    call: &ToolCall,
    now: Timestamp,
    width: usize,
) -> Vec<Line<'static>> {
    let (mark, style) = outcome(call);
    let mut lines = vec![
        Line::styled(call.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Line::from(vec![
            Span::styled(format!("{:<10}", "outcome"), dim()),
            Span::styled(mark, style),
        ]),
        field(
            "session",
            app::label(session)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
        ),
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
    let command = command_lines(call, width);
    if !command.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(command);
    }
    lines
}

fn subagent_lines(
    session: &Session,
    subagent: &Subagent,
    now: Timestamp,
    width: usize,
    chosen: usize,
    focused: bool,
) -> (Vec<Line<'static>>, Option<(usize, bool)>) {
    let mut lines: Vec<Line> = wrap(subagent_name(subagent), width)
        .into_iter()
        .map(|line| line.style(Style::new().add_modifier(Modifier::BOLD)))
        .collect();
    if let Some(kind) = &subagent.agent_type {
        lines.push(field("type", kind.clone()));
    }
    lines.push(field(
        "session",
        app::label(session)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
    ));
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

    let calls = app::calls_of(session, &subagent.agent_id);
    // The call the pane beside this one shows.
    let chosen = (!calls.is_empty()).then(|| chosen.min(calls.len() - 1));
    let cursor = chosen.map(|at| (lines.len() + 2 + at, at == 0));
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "tool calls ({} in all, the latest {})",
            subagent.tool_calls,
            calls.len()
        ),
        dim(),
    ));
    let room = width.saturating_sub(2 + 15 + 1 + 7 + 2 + 7);
    for (index, call) in calls.into_iter().enumerate() {
        let (mark, style) = outcome(call);
        // Marked, and under the cursor while this pane has the focus.
        let (mark_at, row) = match (chosen == Some(index), focused) {
            (true, true) => ("› ", Style::new().add_modifier(Modifier::REVERSED)),
            (true, false) => ("› ", Style::new()),
            _ => ("  ", Style::new()),
        };
        lines.push(
            Line::from(vec![
                Span::raw(format!(
                    "{mark_at}{:<15}",
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
            ])
            .style(row),
        );
    }
    (lines, cursor)
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
            " 1-5 page  j/k move  Enter open  i detail  Esc back  / search  s sort  f filter  ? help  q quit",
            dim(),
        )
    };
    frame.render_widget(line, area);
}

const HELP: &[(&str, &str)] = &[
    (
        "1 2 3 4 5",
        "Sessions / Tool calls / Subagents / Trace / Usage",
    ),
    ("j / k", "down / up (scrolls the detail pane)"),
    ("gg / G", "top / bottom"),
    ("Ctrl+d / Ctrl+u", "half a page down / up"),
    (
        "Enter (session)",
        "narrow pages 2 to 4 to it, show its tool calls",
    ),
    (
        "Enter (subagent)",
        "narrow page 2 to it, show its tool calls",
    ),
    ("i / Enter", "focus the detail pane"),
    (
        "j / k, i / Enter",
        "in a subagent's detail: choose a call, into its pane",
    ),
    ("Tab / Shift+Tab", "the page's panes in turn"),
    ("h / Esc", "back a pane, to the list"),
    (
        "Enter, l / h",
        "in the trace: unfold or fold a turn or a subagent",
    ),
    (
        "Esc (list)",
        "clear the search, then the narrowing, step by step",
    ),
    ("/", "search the list"),
    ("n / N", "next / previous match"),
    ("s", "sort sessions: updated, cost, tokens"),
    ("f", "filter sessions: all, live, open"),
    ("r", "refresh now"),
    ("?", "this help"),
    ("q / Ctrl+c", "quit"),
];

fn draw_help(frame: &mut Frame, area: Rect) {
    let width = 76.min(area.width);
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
                Span::styled(format!(" {keys:<18}"), Style::new().fg(Color::Cyan)),
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
    const SOON: &str = "2026-11-02T10:00:20.000Z";

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

    /// Puts the Sessions page's cursor on a session.
    fn select(app: &mut App, id: &str) {
        app.go_to(Page::Sessions);
        app.leave_detail();
        let at = app.order.iter().position(|other| other == id).unwrap();
        app.move_first();
        app.move_by(at as isize);
        assert_eq!(app.selected_session().unwrap().id, id);
    }

    fn row<'a>(screen: &'a str, needle: &str) -> &'a str {
        screen
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle}\n{screen}"))
    }

    /// The rows the two panes start on.
    fn pane_tops(screen: &str) -> Vec<usize> {
        screen
            .lines()
            .enumerate()
            .filter(|(_, line)| line.starts_with('┌'))
            .map(|(row, _)| row)
            .collect()
    }

    #[test]
    fn the_sessions_page_is_one_list_and_a_detail_pane() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        let screen = screen(&mut app, 150, 44, SOON);

        // Header: the four pages, the counts, the live state.
        let header = screen.lines().next().unwrap();
        for page in [
            "1:Sessions",
            "2:Tool calls",
            "3:Subagents",
            "4:Trace",
            "5:Usage",
        ] {
            assert!(header.contains(page), "{header}");
        }
        assert!(header.contains("3 sessions"));
        assert!(header.contains("live off"));

        // The list has the whole width.
        assert!(screen.contains("Sessions (all · updated ↓)"), "{screen}");
        let listed = row(&screen, "Synthetic session title");
        assert!(listed.contains("example"));
        assert!(listed.contains("main"));
        assert!(listed.contains("idle"));
        assert!(listed.contains("4.4k"));
        assert!(listed.contains("~$0.0089"));
        assert!(row(&screen, "other.repo").contains("active"));
        assert!(row(&screen, "Synthetic title after the damage").contains("example"));

        // The detail of the session under the cursor.
        assert!(screen.contains("┌ Session "));
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
    fn the_tool_calls_page_lists_every_session_or_the_one_entered() {
        let mut app = fixture_app();
        app.go_to(Page::Tools);
        let all = screen(&mut app, 150, 44, SOON);
        assert!(all.contains("Tool calls (5)"), "{all}");
        // Unscoped rows name their session.
        assert!(all
            .lines()
            .any(|l| l.contains("when") && l.contains("session")));
        let bash = row(&all, "cargo test --workspace");
        assert!(bash.contains("22222222"));
        assert!(bash.contains("Bash"));
        assert!(bash.contains("1.5s"));
        assert!(bash.contains("ok"));
        assert!(row(&all, "/work/example/README.md").contains("failed"));
        assert!(row(&all, "synthetic lookup").contains("example/lookup"));
        assert!(row(&all, "synthetic pattern").contains("↳ Grep"));

        // Enter on a session: its calls only, and the title says whose.
        select(&mut app, WITH_SUBAGENTS);
        app.enter();
        let scoped = screen(&mut app, 150, 44, SOON);
        assert!(scoped.contains("Tool calls (5) of 22222222"), "{scoped}");
        assert!(!scoped
            .lines()
            .any(|l| l.contains("when") && l.contains("session")));
        select(&mut app, INDEXED);
        app.enter();
        let none = screen(&mut app, 150, 44, SOON);
        assert!(
            none.contains("Tool calls (0) of Synthetic session title"),
            "{none}"
        );
        assert!(none.contains("no tool calls"));

        // The Sessions page marks the session the others are narrowed to.
        app.go_to(Page::Sessions);
        let sessions = screen(&mut app, 150, 44, SOON);
        assert!(row(&sessions, "Synthetic session title").contains('●'));
        assert!(!row(&sessions, "other.repo").contains('●'));
    }

    #[test]
    fn the_detail_pane_describes_the_selected_tool_call() {
        let mut app = fixture_app();
        app.go_to(Page::Tools);
        app.move_last(); // the oldest call: Bash
        let tool = screen(&mut app, 140, 44, LATER);
        let detail = &tool[tool.find("┌ Tool call ").unwrap()..];
        assert!(detail.contains("Bash"), "{tool}");
        assert!(detail.contains("outcome   ok"));
        assert!(detail.contains("session   22222222"));
        assert!(detail.contains("took      1.5s"));
        assert!(detail.contains("by        the main thread"));
        assert!(detail.contains("toolu_01"));
        assert!(detail.contains("cargo test --workspace"));

        app.move_first();
        app.move_by(1); // a subagent's Grep
        let tool = screen(&mut app, 140, 44, LATER);
        let detail = &tool[tool.find("┌ Tool call ").unwrap()..];
        assert!(detail.contains("by        subagent"), "{tool}");
    }

    #[test]
    fn a_shell_command_is_shown_in_colour() {
        let mut app = fixture_app();
        app.go_to(Page::Tools);
        app.move_last(); // Bash: cargo test --workspace
        let now = Timestamp::parse(LATER).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        // The command is the last line of the detail pane.
        let colours: Vec<(String, Color)> = (0..44)
            .rev()
            .find(|y| buffer[(1, *y)].symbol() == "c")
            .map(|y| {
                (1..23)
                    .map(|x| (buffer[(x, y)].symbol().to_owned(), buffer[(x, y)].fg))
                    .collect()
            })
            .unwrap();
        let text: String = colours.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(text, "cargo test --workspace");
        assert_eq!(colours[0].1, Color::Blue);
        assert_eq!(colours[6].1, Color::Reset);
        assert_eq!(colours[11].1, Color::Yellow);
    }

    #[test]
    fn a_bar_is_where_its_span_is_on_the_axis() {
        let at = |ms: i64| Timestamp::from_millis(ms);
        let axis = (at(0), at(1000));
        let draw = |start: i64, end: Option<i64>| bar(Some(at(start)), end.map(at), axis, 10);
        assert_eq!(draw(0, Some(1000)), "██████████");
        assert_eq!(draw(0, Some(500)), "█████");
        assert_eq!(draw(500, Some(1000)), "     █████");
        assert_eq!(draw(200, Some(400)), "  ██");
        // Too short to see, without an end, or outside the axis: one column.
        assert_eq!(draw(300, Some(301)), "   █");
        assert_eq!(draw(300, None), "   █");
        assert_eq!(draw(1000, Some(2000)), "         █");
        assert_eq!(draw(-500, Some(-100)), "█");
        // Nothing to draw.
        assert_eq!(bar(None, Some(at(5)), axis, 10), "");
        assert_eq!(draw(0, Some(1000)).len(), 30);
        assert_eq!(bar(Some(at(0)), Some(at(5)), axis, 0), "");
        assert_eq!(bar(Some(at(3)), Some(at(3)), (at(3), at(3)), 4), "█");
    }

    /// The column a row's bar starts in.
    fn bar_at(row: &str) -> usize {
        row.chars()
            .position(|c| c == '█')
            .unwrap_or_else(|| panic!("no bar in {row}"))
    }

    #[test]
    fn the_trace_page_shows_a_turn_as_a_waterfall() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        app.enter();
        app.go_to(Page::Trace);

        // Folded: one row a turn, and the turn's detail.
        let folded = screen(&mut app, 150, 44, SOON);
        assert!(folded.contains("Trace (1 turn) of 22222222"), "{folded}");
        let turn = row(&folded, "▸ turn");
        assert!(turn.contains("3 requests, 3 tool calls"), "{turn}");
        assert!(turn.contains("8.0s"));
        assert!(turn.contains("done"));
        assert!(!folded.contains("cargo test --workspace"));
        let detail = &folded[folded.find("┌ Span ").unwrap()..];
        assert!(detail.contains("took      8.0s"), "{folded}");
        assert!(detail.contains("requests  3, 5.5s in all (approximate)"));
        assert!(detail.contains("tools     3, 3.7s in all"));
        assert!(detail.contains("subagents 2"));
        assert!(detail.contains("id        prompt-u1"));

        // Unfolded: its spans, each with a bar where it was in the turn.
        app.enter();
        let open = screen(&mut app, 150, 44, SOON);
        assert!(row(&open, "▾ turn").contains("────"), "{open}");
        let bash = row(&open, "cargo test --workspace");
        assert!(bash.contains("Bash") && bash.contains("1.5s") && bash.contains("ok"));
        assert!(row(&open, "/work/example/README.md").contains("failed"));
        assert!(row(&open, "▸ ↳ Explore").contains("Synthetic agent task"));
        assert!(row(&open, "▸ ↳ agent").contains("b2c3d4e5f6071829"));
        let requests: Vec<&str> = open.lines().filter(|l| l.contains("request  ")).collect();
        assert_eq!(requests.len(), 3, "{open}");
        assert!(requests[0].contains("fable-5-2") && requests[0].contains("tool_use"));
        assert!(requests[2].contains("end_turn"));
        // In the order they happened, left to right.
        assert!(bar_at(requests[0]) < bar_at(bash));
        assert!(bar_at(bash) < bar_at(row(&open, "synthetic lookup")));
        assert!(bar_at(requests[1]) < bar_at(requests[2]));
        // The last request ends where the turn does.
        assert!(requests[2].trim_end_matches('│').ends_with('█'));

        // The detail of a request, of a call and of a subagent.
        app.move_by(1);
        let request = screen(&mut app, 150, 44, SOON);
        let detail = &request[request.find("┌ Span ").unwrap()..];
        assert!(detail.contains("API request"), "{request}");
        assert!(detail.contains("model     claude-fable-5-2"));
        assert!(detail.contains("source    repl_main_thread"));
        assert!(detail.contains("stop      tool_use"));
        assert!(detail.contains("took      2.4s  approximate"));
        assert!(detail.contains("id        msg_1"));
        app.move_by(1);
        let call = screen(&mut app, 150, 44, SOON);
        let detail = &call[call.find("┌ Span ").unwrap()..];
        assert!(detail.contains("by        the main thread"), "{call}");
        assert!(detail.contains("cargo test --workspace"));
        app.move_by(2);
        app.right();
        let subagent = screen(&mut app, 150, 44, SOON);
        assert!(row(&subagent, "▾ ↳ Explore").contains("done"), "{subagent}");
        assert!(row(&subagent, "synthetic pattern").contains("Grep"));
        let detail = &subagent[subagent.find("┌ Span ").unwrap()..];
        assert!(detail.contains("type      Explore"));

        // Without a session entered, rows name their session, and a narrow
        // terminal drops the time axis.
        app.back();
        let all = screen(&mut app, 150, 44, SOON);
        assert!(all.contains("Trace (3 turns)"), "{all}");
        assert!(all
            .lines()
            .any(|l| l.contains("session") && l.contains("time")));
        assert!(row(&all, "Synthetic session tit").contains("▸ turn"));
        let narrow = screen(&mut app, 60, 30, SOON);
        assert!(!narrow.contains('█'), "{narrow}");
        assert!(narrow.contains("turn"));
    }

    #[test]
    fn a_turn_without_an_end_runs_or_was_cut_off() {
        use crate::app::tests::{append_call, append_prompt, scratch_projects};
        let projects = scratch_projects("trace-running");
        append_prompt(&projects, "prompt-new", 0);
        append_call(
            &projects,
            "toolu_new1",
            1,
            "Bash",
            r#"{"command":"sleep 600"}"#,
        );
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
        });
        app.go_to(Page::Trace);
        // Under the turn of a session that is a day later.
        app.move_by(1);
        assert_eq!(app.selected_row().unwrap().turn.id, "prompt-new");
        app.enter();

        // Its session was active a moment ago: everything reaches to now.
        let running = screen(&mut app, 150, 44, "2026-11-02T11:01:30.000Z");
        let turn = row(&running, "▾ turn");
        assert!(
            turn.contains("running") && turn.contains("1m30s"),
            "{running}"
        );
        let call = row(&running, "sleep 600");
        assert!(
            call.contains("running") && call.contains("30.0s"),
            "{running}"
        );
        assert!(call.trim_end_matches('│').ends_with('█'));
        let detail = &running[running.find("┌ Span ").unwrap()..];
        assert!(detail.contains("state     running"));
        assert!(detail.contains("so far, by the records read"));

        // Long after: all that is known is that it has no end.
        let later = screen(&mut app, 150, 44, LATER);
        let turn = row(&later, "▾ turn");
        assert!(turn.contains("no end") && turn.contains("1m00s"), "{later}");
        assert!(!row(&later, "sleep 600")
            .trim_end_matches('│')
            .ends_with("██"));
        assert!(later.contains("no end recorded"));

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn the_subagents_page_lists_them_with_their_cost() {
        let mut app = fixture_app();
        app.go_to(Page::Subagents);
        let screen = screen(&mut app, 150, 44, LATER);
        assert!(screen.contains("Subagents (2)"), "{screen}");
        let explore = row(&screen, "Synthetic agent task");
        assert!(explore.contains("Explore"));
        assert!(explore.contains("22222222"));
        assert!(explore.contains("haiku-5-5"));
        assert!(explore.contains("1.8k"));
        assert!(explore.contains("~$0.0002"));
        assert!(explore.contains("done"));
        // The one without a metadata file: no type, not marked done.
        assert!(row(&screen, "b2c3d4e5f6071829").contains("running"));

        let detail = &screen[screen.find("┌ Subagent ").unwrap()..];
        assert!(detail.contains("session   22222222"));
        assert!(detail.contains("tool calls (1 in all, the latest 1)"));
        assert!(detail.contains("Grep"));
        assert!(detail.contains("synthetic pattern"));
    }

    #[test]
    fn a_call_chosen_in_a_subagents_detail_shows_its_whole_command() {
        use crate::app::tests::{append_agent_call, scratch_projects};
        let projects = scratch_projects("whole-command");
        let long = format!("cargo build {}--the-end", "--synthetic-flag ".repeat(20));
        append_agent_call(
            &projects,
            "toolu_s2",
            0,
            &format!(r#"{{"command":"{long}\\necho second-line"}}"#),
        );
        for (index, minute) in (3..40).zip(1..) {
            let input = format!(r#"{{"command":"echo call-{index}"}}"#);
            append_agent_call(&projects, &format!("toolu_s{index:02}"), minute, &input);
        }
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
        });
        app.go_to(Page::Subagents);

        // The list has the focus: the pane starts at the top, and the one
        // beside it shows the latest call.
        let listed = screen(&mut app, 120, 40, LATER);
        assert!(listed.contains("┌ Subagent [1-"), "{listed}");
        assert!(listed.contains("echo call-39"), "{listed}");
        assert!(listed.contains("tool calls (39 in all, the latest 39)"));

        // In the detail pane the cursor is on a call, and the pane follows
        // it down to the oldest and back.
        app.focus_detail();
        app.move_last();
        let bottom = screen(&mut app, 120, 40, LATER);
        assert!(bottom.contains("synthetic pattern"), "{bottom}");
        assert!(!bottom.contains("call-39"));
        assert!(row(&bottom, "› Grep").contains("synthetic pattern"));
        app.move_by(-1);
        let cut = screen(&mut app, 120, 40, LATER);
        assert!(cut.contains("cargo build --synthetic-flag"), "{cut}");
        // The pane beside it shows that call, with every line of the
        // command.
        let call = &cut[cut.find("┌ Tool call ").unwrap()..];
        assert!(call.contains("Bash  running"), "{cut}");
        assert!(call.contains("--the-end"), "{cut}");
        assert!(call.contains("echo second-line"));
        assert!(call.contains("toolu_s2"));
        assert!(row(&cut, "› Bash").contains("cargo build"));

        // Enter: into that pane, which scrolls when the command is long.
        app.enter();
        assert!(app.in_call());
        screen(&mut app, 120, 14, LATER);
        assert!(app.viewport.call_lines > app.viewport.call_rows);
        app.move_last();
        let scrolled = screen(&mut app, 120, 14, LATER);
        assert!(scrolled.contains("┌ Tool call ["), "{scrolled}");
        assert!(scrolled.contains("toolu_s2"));
        app.back();

        // Back at the top of the subagent's calls, the pane shows its head.
        app.move_first();
        let top = screen(&mut app, 120, 40, LATER);
        assert!(top.contains("┌ Subagent [1-"), "{top}");
        assert!(top.contains("call-39"));

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn entering_a_subagent_shows_its_tool_calls_and_their_detail() {
        let mut app = fixture_app();
        app.go_to(Page::Subagents);
        app.enter();
        let screen = screen(&mut app, 150, 40, LATER);
        assert!(
            screen.contains("Tool calls (1) of 22222222 › Explore: Synthetic agent task"),
            "{screen}"
        );
        assert!(row(&screen, "synthetic pattern").contains("↳ Grep"));
        let detail = &screen[screen.find("┌ Tool call ").unwrap()..];
        assert!(detail.contains("by        subagent Explore (Synthetic agent task)"));
        assert!(detail.contains("took      400ms"));

        // Its page marks it.
        app.go_to(Page::Subagents);
        let subagents = super::tests::screen(&mut app, 150, 40, LATER);
        assert!(row(&subagents, "Synthetic agent task").contains("● Explore"));
    }

    #[test]
    fn a_list_scrolls_only_when_the_cursor_leaves_it() {
        let mut app = fixture_app();
        app.go_to(Page::Tools);
        // 12 rows: a list pane of 2 visible rows (45% of 10, less the border
        // and the header).
        screen(&mut app, 120, 12, LATER);
        assert_eq!(app.viewport.list_rows, 1.max(app.viewport.list_rows));
        let visible = app.viewport.list_rows;
        assert!(visible < 5, "the list must not fit: {visible}");
        app.move_last();
        screen(&mut app, 120, 12, LATER);
        let bottom = app.offsets[1];
        assert_eq!(bottom, 5 - visible);
        // Moving up within what is visible does not scroll.
        if visible > 1 {
            app.move_by(-1);
            screen(&mut app, 120, 12, LATER);
            assert_eq!(app.offsets[1], bottom);
        }
        app.move_first();
        screen(&mut app, 120, 12, LATER);
        assert_eq!(app.offsets[1], 0);
    }

    #[test]
    fn long_text_is_wrapped_in_the_detail_pane_not_cut() {
        let text =
            |lines: Vec<Line>| -> Vec<String> { lines.iter().map(|l| l.to_string()).collect() };
        assert_eq!(
            text(wrap("cargo test --workspace --all-features", 16)),
            ["cargo test ", "--workspace ", "--all-features"]
        );
        // A word wider than the pane is cut where it has to be.
        assert_eq!(
            text(wrap("/a/very/long/path/to/a/file.rs", 10)),
            ["/a/very/lo", "ng/path/to", "/a/file.rs"]
        );
        // Wide characters count for two columns.
        assert_eq!(text(wrap("日本語のテキスト", 8)), ["日本語の", "テキスト"]);
        assert_eq!(text(wrap("short", 80)), ["short"]);
        assert_eq!(text(wrap("", 10)), Vec::<String>::new());
        for width in 0..4 {
            wrap("anything at all", width);
        }
    }

    #[test]
    fn the_layout_does_not_depend_on_what_the_panes_hold() {
        let mut app = fixture_app();
        // 40 rows: a header, a body of 38 split 45% / 55%, a footer.
        let expected = vec![1, 18];
        for id in [WITH_SUBAGENTS, INDEXED] {
            select(&mut app, id);
            assert_eq!(pane_tops(&screen(&mut app, 120, 40, LATER)), expected);
            app.focus_detail();
            assert_eq!(pane_tops(&screen(&mut app, 120, 40, LATER)), expected);
        }
        for page in [Page::Tools, Page::Subagents] {
            app.go_to(page);
            assert_eq!(pane_tops(&screen(&mut app, 120, 40, LATER)), expected);
        }
        // Narrowed to a session with nothing to list: the same.
        select(&mut app, INDEXED);
        app.enter();
        assert_eq!(pane_tops(&screen(&mut app, 120, 40, LATER)), expected);
        // And at another width.
        assert_eq!(pane_tops(&screen(&mut app, 70, 40, LATER)), expected);
    }

    #[test]
    fn the_detail_pane_scrolls_and_stops_at_its_end() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        // Short: the session's detail does not fit.
        let first = screen(&mut app, 120, 20, LATER);
        assert!(first.contains("Session [1-"), "{first}");
        assert!(!first.contains("by source"));

        app.focus_detail();
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
        // Scrolling the detail pane does not move the list.
        assert_eq!(app.selected_session().unwrap().id, WITH_SUBAGENTS);
    }

    #[test]
    fn a_search_marks_its_matches_in_the_title_and_moves_between_them() {
        let mut app = fixture_app();
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

        // A search belongs to its page.
        app.go_to(Page::Tools);
        app.start_search();
        app.typing = Some("cargo".to_owned());
        app.submit_search();
        let tools = screen(&mut app, 140, 30, LATER);
        assert!(tools.contains("Tool calls (5)  /cargo (1)"), "{tools}");
        assert!(!tools.contains("/synthetic"));
        assert_eq!(app.selected_tool().unwrap().1.name, "Bash");

        // Esc clears the search first.
        app.back();
        assert_eq!(app.search, None);
        // There is no searching from inside the detail pane.
        app.focus_detail();
        app.start_search();
        assert_eq!(app.typing, None);
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
        assert!(live.contains("no sessions"));
        // The other pages are not filtered by it.
        app.go_to(Page::Tools);
        assert!(screen(&mut app, 140, 30, LATER).contains("Tool calls (5)"));

        app.go_to(Page::Sessions);
        app.cycle_filter();
        assert_eq!(app.filter, Filter::Open);
        assert_eq!(app.order.len(), 3);
    }

    #[test]
    fn the_usage_page_totals_every_session() {
        let mut app = fixture_app();
        app.go_to(Page::Usage);
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
        assert!(screen.contains("narrow pages 2 to 4"));
        assert!(screen.contains("Sessions / Tool calls / Subagents / Trace / Usage"));
        assert!(screen.contains("unfold or fold a turn"));
    }

    #[test]
    fn nothing_panics_at_small_sizes() {
        let mut app = fixture_app();
        app.help = true;
        for page in Page::ALL {
            app.go_to(page);
            // Unfolded on the Trace page; the detail pane elsewhere.
            app.enter();
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
        let sessions = screen(&mut app, 120, 20, LATER);
        assert!(sessions.contains("0 sessions"), "{sessions}");
        assert!(sessions.contains("no projects directory"));
        for page in [Page::Tools, Page::Subagents, Page::Trace, Page::Usage] {
            app.go_to(page);
            screen(&mut app, 120, 20, LATER);
        }
    }

    #[test]
    #[ignore = "prints the screen for a human to look at"]
    fn show_the_screen() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        println!("{}", screen(&mut app, 150, 36, SOON));
        app.go_to(Page::Tools);
        println!("{}", screen(&mut app, 150, 24, SOON));
        app.go_to(Page::Subagents);
        println!("{}", screen(&mut app, 150, 30, SOON));
        app.focus_detail();
        println!("{}", screen(&mut app, 150, 30, SOON));
        println!("{}", screen(&mut app, 100, 30, SOON));
        app.go_to(Page::Sessions);
        app.enter();
        println!("{}", screen(&mut app, 100, 24, SOON));
        app.go_to(Page::Trace);
        app.enter();
        println!("{}", screen(&mut app, 150, 30, SOON));
        app.move_by(3);
        app.enter();
        println!("{}", screen(&mut app, 150, 30, SOON));
        app.back();
        app.back();
        println!("{}", screen(&mut app, 110, 24, SOON));
    }

    #[test]
    fn no_unmodelled_text_reaches_the_screen() {
        let mut app = fixture_app();
        for page in Page::ALL {
            app.go_to(page);
            if page == Page::Trace {
                // Every turn and every subagent unfolded.
                for at in (0..40).rev() {
                    app.move_first();
                    app.move_by(at);
                    app.right();
                    app.leave_detail();
                }
                app.in_detail = false;
                app.move_first();
            }
            for _ in 0..6 {
                let screen = screen(&mut app, 160, 50, LATER);
                assert!(!screen.contains("SYNTHETIC-TEXT"), "{screen}");
                app.move_by(1);
            }
        }
    }
}

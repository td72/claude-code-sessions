//! Drawing. Each list page is a list over a detail pane in fixed proportions
//! (the shape of vig's pages); the Usage page is three tables.

use std::collections::BTreeMap;

use iktinos::record::Timestamp;
use iktinos::session::{CostSource, LiveState, ModelUsage, Session, Subagent, ToolCall};
use iktinos::trace::{between, Interaction, Item, LlmRequest, SpanKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::{
    self, App, InteractionRow, LiveStatus, Loaded, Page, Status, StatusKind, TimeStyle, Times,
    TraceItem, TraceRow, TREE,
};
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
            Page::Subagents => draw_subagent_list(frame, list, app, now),
            _ => {
                // The interactions, and beside them what is chosen: the interaction, or
                // the span chosen in its tree below.
                let [interactions, span] =
                    Layout::horizontal([Constraint::Percentage(60), Constraint::Min(0)])
                        .areas(list);
                draw_interaction_list(frame, interactions, app, now);
                draw_span(frame, span, app, now);
            }
        }
        match app.page {
            Page::Subagents => {
                // The subagent, and beside it the tool call chosen in it.
                // The subagent's side is as wide as its tables, where there
                // is room.
                let left = 72.min(detail.width * 11 / 20);
                let [subagent, call] =
                    Layout::horizontal([Constraint::Length(left), Constraint::Min(0)])
                        .areas(detail);
                draw_detail(frame, subagent, app, now);
                draw_call(frame, call, app, now);
            }
            Page::Trace => draw_tree(frame, detail, app, now),
            _ => draw_detail(frame, detail, app, now),
        }
    } else {
        draw_usage_page(frame, body, app);
    }
    draw_footer(frame, footer, app);
    if app.help {
        draw_help(frame, frame.area(), app);
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
    spans.push(live_span(app, now, counts[0] > 0));
    frame.render_widget(Line::from(spans), area);
}

/// How long the socket may be silent while a session is busy before the
/// header says so in colour.
const QUIET: i64 = 60_000;

/// The header's live part: whether the viewer listens, and what it has
/// heard. An open socket alone says nothing about a mod that sends.
fn live_span(app: &App, now: Timestamp, busy: bool) -> Span<'static> {
    let warn = Style::new().fg(Color::Yellow);
    match &app.live {
        LiveStatus::Off => Span::styled("live off", dim()),
        LiveStatus::Failed(_) => Span::styled("live failed", Style::new().fg(Color::Red)),
        LiveStatus::Listening(_) => match app.received.last_at {
            None => Span::styled("live on, no events yet", warn),
            Some(at) => {
                let quiet = busy && now.as_millis() - at.as_millis() > QUIET;
                Span::styled(
                    format!(
                        "live {}, last {}",
                        events(app.received.events),
                        when_said(app.times(), now, at)
                    ),
                    if quiet {
                        warn
                    } else {
                        Style::new().fg(Color::Green)
                    },
                )
            }
        },
    }
}

fn events(count: u64) -> String {
    match count {
        1 => "1 event".to_owned(),
        _ => format!("{count} events"),
    }
}

/// [`when`] in a sentence: `3s ago`, `at 10:00:08`.
fn when_said(times: Times, now: Timestamp, at: Timestamp) -> String {
    let age = now.as_millis() - at.as_millis();
    let new = age >= 0 && (age as u128) < times.ago_within.as_millis();
    match times.style {
        TimeStyle::Clock if !new => format!("at {}", format::when(now, at)),
        _ => match format::ago(now, at).as_str() {
            "now" => "now".to_owned(),
            ago => format!("{ago} ago"),
        },
    }
}

/// What the socket has received, for the help: where it listens, and the
/// signs of a mod and a viewer that do not match.
fn live_lines(app: &App) -> Vec<String> {
    match &app.live {
        LiveStatus::Off => Vec::new(),
        LiveStatus::Failed(reason) => vec![format!("unavailable: {reason}")],
        LiveStatus::Listening(socket) => {
            let received = &app.received;
            vec![
                socket.display().to_string(),
                format!(
                    "{}, {} of an unknown type, {} rejected",
                    events(received.events),
                    received.unknown,
                    match received.rejected {
                        1 => "1 request".to_owned(),
                        n => format!("{n} requests"),
                    }
                ),
            ]
        }
    }
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

/// When something happened, the way the lists are set to say it: how
/// long ago, or the date and time once it is no longer new.
fn when(times: Times, now: Timestamp, at: Timestamp) -> String {
    let age = now.as_millis() - at.as_millis();
    let new = age >= 0 && (age as u128) < times.ago_within.as_millis();
    match times.style {
        TimeStyle::Clock if !new => format::when(now, at),
        _ => format::ago(now, at),
    }
}

/// The width of the column [`when`] fills.
fn when_width(times: Times) -> u16 {
    match times.style {
        TimeStyle::Clock => format::WHEN_WIDTH,
        TimeStyle::Relative => 4,
    }
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
                Cell::from(app::project(session).unwrap_or_else(|| "—".to_owned()))
                    .style(cwd_style()),
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
        None => ("running", running()),
    }
}

fn draw_tool_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let time = app.times();
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
                .map_or("—".to_owned(), |at| when(time, now, at));
            let mut cells = vec![Cell::from(Line::from(when).right_aligned())];
            if !scoped {
                cells.push(session_cell(session));
            }
            cells.extend([
                Cell::from(name).style(tool_style()),
                Cell::from(call.summary.clone().unwrap_or_default()),
                Cell::from(
                    Line::from(call.duration_ms.map(format::duration).unwrap_or_default())
                        .right_aligned(),
                )
                .style(took_style(call.duration_ms)),
                Cell::from(mark).style(style),
            ]);
            Row::new(cells).style(row_style(&matches, index))
        })
        .collect();
    // What the call was about gets every column the others can spare.
    let mut widths = vec![Constraint::Length(when_width(time))];
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

/// A status in the one word a narrow column has room for: `running` for
/// `running Bash`, `approve?` for `approve Write?`.
fn status_word(status: &Status) -> String {
    match status.kind {
        StatusKind::NeedsApproval => "approve?".to_owned(),
        _ => status.text.split(' ').next().unwrap_or_default().to_owned(),
    }
}

fn subagent_name(subagent: &Subagent) -> &str {
    subagent
        .description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&subagent.agent_id)
}

fn draw_subagent_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let time = app.times();
    let matches = app.matches(Page::Subagents, now);
    let scoped = app.scope.is_some();
    let subagents = app.subagents();
    let count = subagents.len();
    let rows: Vec<Row<'static>> = subagents
        .iter()
        .enumerate()
        .map(|(index, (session, subagent))| {
            let done = match subagent.finished {
                Some(true) => Span::styled("done", done()),
                _ => Span::styled("running", running()),
            };
            let when = subagent
                .last_activity
                .map_or("—".to_owned(), |at| when(time, now, at));
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
                ))
                .style(agent_style()),
                Cell::from(subagent_name(subagent).to_owned()),
                Cell::from(
                    subagent
                        .model
                        .as_deref()
                        .map_or("—".to_owned(), format::model_name),
                ),
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
    let mut widths = vec![Constraint::Length(when_width(time))];
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

/// Whether an interaction is still going: it has no end, it is its session's latest
/// and the session is at work. The transcript alone cannot tell an interaction that
/// runs from one that was cut off.
fn interaction_runs(session: &Session, interaction: &Interaction, now: Timestamp) -> bool {
    interaction.ended_at.is_none()
        && session
            .trace
            .back()
            .is_some_and(|last| std::ptr::eq(last, interaction))
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

/// What an interaction's main thread did, in a few words.
fn interaction_summary(interaction: &Interaction) -> String {
    let spans = &interaction.thread.spans;
    let calls = spans
        .iter()
        .filter(|span| matches!(span.kind, SpanKind::Tool(_)))
        .count();
    format!(
        "{}, {}",
        format::count((spans.len() - calls) as u64, "LLM request"),
        format::count(calls as u64, "tool call")
    )
}

/// What an interaction's row says it was: what was asked, or, where the
/// prompt is not known, the tools it used and how often (`Bash×3 Read×2`).
fn interaction_text(interaction: &Interaction) -> (String, Style) {
    if let Some(prompt) = &interaction.prompt {
        return (prompt.clone(), Style::new());
    }
    // In the order they were first used.
    let mut tools: Vec<(String, usize)> = Vec::new();
    for span in &interaction.thread.spans {
        if let SpanKind::Tool(call) = &span.kind {
            let name = format::tool_name(&call.name);
            match tools.iter_mut().find(|(known, _)| *known == name) {
                Some((_, count)) => *count += 1,
                None => tools.push((name, 1)),
            }
        }
    }
    let text = tools
        .iter()
        .map(|(name, count)| match count {
            1 => name.clone(),
            _ => format!("{name}×{count}"),
        })
        .collect::<Vec<_>>()
        .join(" ");
    (text, tool_style())
}

/// How an interaction ended, as far as can be told.
fn interaction_state(
    session: &Session,
    interaction: &Interaction,
    now: Timestamp,
) -> (&'static str, Style) {
    if interaction.ended_at.is_some() {
        ("done", done())
    } else if interaction_runs(session, interaction, now) {
        ("running", running())
    } else {
        ("no end", dim())
    }
}

fn draw_interaction_list(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let time = app.times();
    let matches = app.matches(Page::Trace, now);
    // Flat, the rows name their session unless the page is narrowed to one;
    // by session, the session is the row they are under, in that column.
    let names = app.by_session || app.scope.is_none();
    let by_session = app.by_session;
    // How many LLM requests and tool calls a main thread holds.
    let counts = |interaction: &Interaction| {
        let spans = &interaction.thread.spans;
        let calls = spans
            .iter()
            .filter(|span| matches!(span.kind, SpanKind::Tool(_)))
            .count();
        (spans.len() - calls, calls)
    };
    let number = |n: usize| Cell::from(Line::from(n.to_string()).right_aligned());
    let listed = app.trace_list();
    let count = listed.len();
    let rows: Vec<Row<'static>> = listed
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let session = row.session;
            let Some(InteractionRow {
                interaction, end, ..
            }) = row.interaction
            else {
                // A session: its name, where it runs, what it is doing.
                let status = app::status(session, now);
                let when = session
                    .trace
                    .iter()
                    .filter_map(|i| i.started_at)
                    .max()
                    .map_or("—".to_owned(), |at| when(time, now, at));
                let name = app::label(session).lines().next().unwrap_or_default();
                return Row::new([
                    Cell::from(Line::from(when).right_aligned()),
                    Cell::from(Line::from(vec![
                        Span::raw(if row.open { "▾ " } else { "▸ " }),
                        // Where it runs first: what tells sessions apart.
                        Span::styled(
                            app::project(session).unwrap_or_else(|| "—".to_owned()),
                            cwd_style(),
                        ),
                        Span::raw("  "),
                        Span::styled(name.to_owned(), Style::new().fg(Color::Cyan)),
                    ])),
                    number(session.trace.iter().map(|i| counts(i).0).sum()),
                    number(session.trace.iter().map(|i| counts(i).1).sum()),
                    Cell::from(""),
                    Cell::from(status_word(&status)).style(style_of(status.kind)),
                ])
                .style(row_style(&matches, index));
            };
            let end = if interaction_runs(session, interaction, now) {
                Some(now)
            } else {
                end
            };
            let took = interaction
                .duration_ms
                .or_else(|| between(interaction.started_at, end));
            let (state, style) = interaction_state(session, interaction, now);
            let when = interaction
                .started_at
                .map_or("—".to_owned(), |at| when(time, now, at));
            let mut cells = vec![Cell::from(Line::from(when).right_aligned())];
            let (text, text_style) = interaction_text(interaction);
            if by_session {
                // Under its session, in the session's column.
                cells.push(Cell::from(Line::from(vec![
                    Span::styled("  └ ", dim()),
                    Span::styled(text, text_style),
                ])));
            } else {
                if names {
                    // Where it runs, then its name.
                    cells.push(Cell::from(Line::from(vec![
                        Span::styled(
                            app::project(session).unwrap_or_else(|| "—".to_owned()),
                            cwd_style(),
                        ),
                        Span::raw("  "),
                        Span::raw(
                            app::label(session)
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .to_owned(),
                        ),
                    ])));
                }
                cells.push(Cell::from(text).style(text_style));
            }
            let (requests, calls) = counts(interaction);
            cells.extend([
                number(requests),
                number(calls),
                Cell::from(
                    Line::from(took.map(format::duration).unwrap_or_default()).right_aligned(),
                )
                .style(took_style(took)),
                Cell::from(state).style(style),
            ]);
            Row::new(cells).style(row_style(&matches, index))
        })
        .collect();
    let mut widths = vec![Constraint::Length(when_width(time))];
    let mut header = vec!["when"];
    if by_session {
        widths.push(Constraint::Fill(1));
        header.push("session / prompt");
    } else {
        if names {
            widths.push(Constraint::Fill(2));
            header.push("session");
        }
        // The session and what was asked share what the short columns
        // leave, the prompt with the larger part.
        widths.push(Constraint::Fill(3));
        header.push("prompt");
    }
    widths.extend([Constraint::Length(4), Constraint::Length(5)]);
    header.extend(["llm", "tools"]);
    widths.extend([Constraint::Length(7), Constraint::Length(8)]);
    header.extend(["took", ""]);
    let title = list_title(
        app,
        format!(
            "Trace ({} · {})",
            format::count(app.interaction_count() as u64, "interaction"),
            if app.by_session {
                "by session"
            } else {
                "latest first"
            }
        ),
        matches.len(),
    );
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .block(pane_block(title, !app.in_detail));
    let selected = (count > 0).then_some(app.trace_selected);
    draw_list(frame, area, app, Page::Trace.list_index(), table, selected);
}

/// The pane beside the interactions: the interaction or the session under the cursor, or,
/// with the focus in an interaction's tree, the span chosen there.
fn draw_span(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    let width = area.width.saturating_sub(2) as usize;
    let chosen = app.selected_span().filter(|_| app.in_tree());
    let under = app.selected_list_row();
    let (title, lines) = match (chosen, under.map(|row| (row.session, row.interaction))) {
        (Some(row), _) => ("Span", trace_lines(app, &row, now, width)),
        // A session's row: the session, as the Sessions page describes it.
        (None, Some((session, None))) => ("Session", session_lines(app, session, now, width)),
        (
            None,
            Some((
                _,
                Some(InteractionRow {
                    session,
                    interaction,
                    end,
                }),
            )),
        ) => {
            // The interaction's end with its subagents, where it has spans.
            let end = app
                .span_rows()
                .first()
                .and_then(|row| row.interaction_end)
                .or(end);
            (
                "Interaction",
                interaction_lines(session, interaction, end, now),
            )
        }
        (None, None) => (
            "Interaction",
            vec![Line::styled(
                app.notice
                    .clone()
                    .unwrap_or_else(|| "no interactions".to_owned()),
                dim(),
            )],
        ),
    };
    let rows = area.height.saturating_sub(2) as usize;
    app.viewport.call_rows = rows;
    app.viewport.call_lines = lines.len();
    app.call_scroll = app.call_scroll.min(lines.len().saturating_sub(rows));
    let title = if lines.len() > rows {
        format!(
            "{title} [{}-{}/{}]",
            app.call_scroll + 1,
            (app.call_scroll + rows).min(lines.len()),
            lines.len()
        )
    } else {
        title.to_owned()
    };
    let paragraph = Paragraph::new(lines)
        .scroll((u16::try_from(app.call_scroll).unwrap_or(u16::MAX), 0))
        .block(pane_block(title, app.in_call()));
    frame.render_widget(paragraph, area);
}

/// What a row of the interaction's tree is called, and what is said beside it.
fn trace_label(row: &TraceRow) -> (String, Style, String) {
    match row.item {
        TraceItem::Span(span) => match &span.kind {
            SpanKind::Tool(call) => (
                format::tool_name(&call.name),
                tool_style(),
                call.summary.clone().unwrap_or_default(),
            ),
            SpanKind::LlmRequest(request) => (
                "llm_request".to_owned(),
                Style::new().fg(Color::LightBlue),
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
            agent_style(),
            subagent_name(subagent).to_owned(),
        ),
    }
}

/// The pane under the interactions: the interaction under the cursor as a tree, each row
/// with a bar on a time axis as long as the interaction.
fn draw_tree(frame: &mut Frame, area: Rect, app: &mut App, now: Timestamp) {
    app.viewport.detail_rows = (area.height as usize).saturating_sub(3);
    let inner = area.width.saturating_sub(2) as usize;
    // The time axis takes two fifths of the pane, where there is room.
    let axis = if inner >= 70 { inner * 2 / 5 } else { 0 };
    let listed = app.span_rows();
    let count = listed.len();
    let rows: Vec<Row<'static>> = listed
        .iter()
        .map(|row| {
            let runs = interaction_runs(row.session, row.interaction, now);
            // The axis: the interaction from its start to its end, or to now.
            let window =
                row.interaction
                    .started_at
                    .zip(if runs { Some(now) } else { row.interaction_end });
            let (start, end) = match row.item {
                TraceItem::Span(span) => (span.started_at, span.ended_at),
                TraceItem::Subagent(subagent) => (subagent.started_at, subagent.last_activity),
            };
            // What has not ended reaches to now while the interaction runs.
            let end = end.or(runs.then_some(now));
            let (mark, mark_style, colour) = match row.item {
                TraceItem::Span(span) => match &span.kind {
                    SpanKind::Tool(call) => {
                        let (mark, style) = outcome(call);
                        let colour = if call.success == Some(false) {
                            Color::Red
                        } else {
                            Color::Green
                        };
                        (mark.to_owned(), style, colour)
                    }
                    SpanKind::LlmRequest(request) => {
                        let stop = request.stop_reason.clone().unwrap_or_default();
                        // The end of the answer, more to do, or cut short.
                        let style = match stop.as_str() {
                            "end_turn" => done(),
                            "tool_use" | "" => dim(),
                            _ => Style::new().fg(Color::Red),
                        };
                        (stop, style, Color::LightBlue)
                    }
                    _ => (String::new(), dim(), Color::LightBlue),
                },
                TraceItem::Subagent(subagent) => match subagent.finished {
                    Some(true) => ("done".to_owned(), done(), Color::Magenta),
                    _ => ("running".to_owned(), running(), Color::Magenta),
                },
            };
            let (name, style, beside) = trace_label(row);
            let fold = match row.open {
                Some(true) => "▾ ",
                Some(false) => "▸ ",
                None => "  ",
            };
            let mut cells = vec![
                Cell::from(Line::from(vec![
                    Span::raw(format!("{}{fold}", "  ".repeat(row.depth))),
                    Span::styled(name, style),
                    Span::raw(format!("  {beside}")),
                ])),
                Cell::from(
                    Line::from(
                        between(start, end)
                            .map(format::duration)
                            .unwrap_or_default(),
                    )
                    .right_aligned(),
                )
                .style(took_style(between(start, end))),
                Cell::from(mark).style(mark_style),
            ];
            if axis > 0 {
                let drawn = window.map_or(String::new(), |window| bar(start, end, window, axis));
                cells.push(Cell::from(drawn).style(Style::new().fg(colour)));
            }
            Row::new(cells)
        })
        .collect();

    let title = match app.selected_interaction() {
        Some(InteractionRow { interaction, .. }) => {
            format!("Interaction: {}", interaction_summary(interaction))
        }
        None => "Interaction".to_owned(),
    };
    let block = pane_block(title, app.choosing_span());
    if count == 0 {
        let text = if app.selected_interaction().is_some() {
            "nothing was read of this interaction but its prompt"
        } else if app.selected_list_row().is_some() {
            "a session: the tree is of one of its interactions"
        } else {
            "no interactions"
        };
        frame.render_widget(Paragraph::new(Line::styled(text, dim())).block(block), area);
        return;
    }
    let mut widths = vec![
        Constraint::Fill(1),
        Constraint::Length(7),
        Constraint::Length(8),
    ];
    let mut header = vec!["span", "took", ""];
    if axis > 0 {
        widths.push(Constraint::Length(axis as u16));
        header.push("time");
    }
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim()))
        .block(block);
    // The cursor shows once the tree has been entered.
    let selected = app.in_tree().then_some(app.span_selected.min(count - 1));
    draw_list(frame, area, app, TREE, table, selected);
}

/// The detail of a row of the interaction's tree.
fn trace_lines(app: &App, row: &TraceRow, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    match row.item {
        TraceItem::Span(span) => match &span.kind {
            SpanKind::Tool(call) => {
                let mut lines = tool_lines(row.session, call, now, width);
                let whole = app.whole_for(&row.session.id, &call.id);
                lines.extend(output_lines(call, whole, width));
                lines
            }
            SpanKind::LlmRequest(request) => {
                llm_request_lines(request, span.started_at, span.duration_ms(), now)
            }
            _ => Vec::new(),
        },
        TraceItem::Subagent(subagent) => {
            subagent_lines(row.session, subagent, now, width, 0, false).0
        }
    }
}

/// An interaction: how long it took and where the time went.
fn interaction_lines(
    session: &Session,
    interaction: &Interaction,
    end: Option<Timestamp>,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let runs = interaction_runs(session, interaction, now);
    let end = if runs { Some(now) } else { end };
    let mut lines = vec![
        Line::styled("Interaction", headline()),
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
            match (interaction.ended_at.is_some(), runs) {
                (true, _) => "done",
                (false, true) => "running",
                (false, false) => "no end recorded: cut off, or not read to its end",
            }
            .to_owned(),
        ),
    ];
    if let Some(path) = session.project_path.as_ref().or(session.cwd.as_ref()) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<10}", "cwd"), label()),
            Span::styled(path.display().to_string(), cwd_style()),
        ]));
    }
    if let Some(prompt) = &interaction.prompt {
        lines.push(field("prompt", prompt.clone()));
    }
    if let Some(at) = interaction.started_at {
        lines.push(field("started", format::started(now, at)));
    }
    let took = interaction
        .duration_ms
        .or_else(|| between(interaction.started_at, end));
    if let Some(ms) = took {
        let note = if interaction.duration_ms.is_some() {
            ""
        } else {
            " so far, by the records read"
        };
        lines.push(field("took", format!("{}{note}", format::duration(ms))));
    }

    // Where the time went, on the main thread; a subagent's time is inside
    // the call that spawned it.
    let (mut requests, mut request_ms, mut calls, mut call_ms) = (0u64, 0u64, 0u64, 0u64);
    for span in &interaction.thread.spans {
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
        "llm",
        format!(
            "{requests} requests, {} in all (approximate)",
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
    for node in session.walk(interaction) {
        match node.item {
            Item::Subagent(_) => subagents += 1,
            Item::Span(span) => {
                if let SpanKind::LlmRequest(request) = &span.kind {
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
    if interaction.thread.dropped > 0 {
        lines.push(field(
            "",
            format!("{} more spans were not kept", interaction.thread.dropped),
        ));
    }
    if !interaction.id.is_empty() {
        lines.push(field("id", interaction.id.clone()));
    }
    lines
}

/// One API response.
fn llm_request_lines(
    request: &LlmRequest,
    started_at: Option<Timestamp>,
    took: Option<u64>,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled("LLM request", headline())];
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
        lines.push(field("started", format::started(now, at)));
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
                Some((session, call)) => {
                    let mut lines = tool_lines(session, call, now, width);
                    let whole = app.whole_for(&session.id, &call.id);
                    lines.extend(output_lines(call, whole, width));
                    lines
                }
                None => empty("no tool calls"),
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
                Some(session) => session_lines(app, session, now, width),
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
        Some(call) => {
            let mut lines = call_lines(call, now, width);
            let whole = app
                .selected_subagent()
                .and_then(|(session, _)| app.whole_for(&session.id, &call.id));
            lines.extend(output_lines(call, whole, width));
            lines
        }
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

/// The most of a whole output that is laid out to be shown, in characters:
/// more than anyone scrolls through, and cheap to wrap again on every draw.
const WHOLE_SHOWN: usize = 200_000;

/// What a tool call returned: the excerpt the session keeps, or the whole
/// output where it was read on demand. Empty for a call without any.
fn output_lines(call: &ToolCall, whole: Option<&Loaded>, width: usize) -> Vec<Line<'static>> {
    let heading = |text: String| Line::styled(text, label());
    let note = |text: String| Line::styled(text, dim());
    let stderr = |lines: &mut Vec<Line<'static>>, text: &str| {
        lines.push(Line::raw(""));
        lines.push(Line::styled("stderr", Style::new().fg(Color::Red)));
        lines.extend(wrap(text, width));
    };
    let mut lines = vec![Line::raw("")];
    match whole.map(|loaded| &loaded.output) {
        Some(Err(reason)) => {
            lines.push(Line::styled(
                format!("output    could not be read: {reason}"),
                Style::new().fg(Color::Red),
            ));
        }
        Some(Ok(None)) => lines.push(note(
            "output    the session's files hold no result of this call".to_owned(),
        )),
        Some(Ok(Some(output))) => {
            let chars = output.text.chars().count();
            // The file it was moved to, by its place in the session's
            // directory.
            let from = match &output.file {
                Some(path) => {
                    let tail: Vec<_> = path.iter().rev().take(2).collect();
                    let tail: std::path::PathBuf = tail.into_iter().rev().collect();
                    format!(", from {}", tail.display())
                }
                None => String::new(),
            };
            lines.push(heading(format!(
                "output    all of it: {}, {}{from}  (o: the excerpt)",
                format::count(output.text.lines().count() as u64, "line"),
                format::count(chars as u64, "character"),
            )));
            let shown = match output.text.char_indices().nth(WHOLE_SHOWN) {
                Some((end, _)) => &output.text[..end],
                None => &output.text,
            };
            lines.extend(wrap(shown, width));
            if chars > WHOLE_SHOWN {
                lines.push(note(format!(
                    "… the first {WHOLE_SHOWN} characters are shown"
                )));
            }
            if output.truncated {
                lines.push(note("… cut where reading stopped".to_owned()));
            }
            if let Some(text) = &output.stderr {
                stderr(&mut lines, text);
            }
            if output.interrupted {
                lines.push(Line::styled("interrupted", running()));
            }
            if output.other_blocks > 0 {
                lines.push(note(format!(
                    "and {} that is not text",
                    format::count(output.other_blocks as u64, "block")
                )));
            }
        }
        None => {
            let Some(output) = &call.output else {
                return Vec::new();
            };
            let size = format!(
                "{}, {}",
                format::count(output.lines as u64, "line"),
                format::count(output.chars as u64, "character")
            );
            lines.push(heading(if output.truncated {
                format!("output    {size}; the start is shown  (o: all of it)")
            } else {
                format!("output    {size}")
            }));
            lines.extend(wrap(&output.text, width));
            if output.truncated {
                lines.push(note("…".to_owned()));
            }
            if let Some(text) = &output.stderr {
                stderr(&mut lines, text);
            }
            if output.interrupted {
                lines.push(Line::styled("interrupted", running()));
            }
            if output.other_blocks > 0 {
                lines.push(note(format!(
                    "and {} that is not text",
                    format::count(output.other_blocks as u64, "block")
                )));
            }
        }
    }
    lines
}

/// A tool call in the pane beside its subagent: the command first.
fn call_lines(call: &ToolCall, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    let (mark, style) = outcome(call);
    let mut head = vec![
        Span::styled(call.name.clone(), headline()),
        Span::raw("  "),
        Span::styled(mark, style),
    ];
    if let Some(ms) = call.duration_ms {
        head.push(Span::styled(
            format!("  took {}", format::duration(ms)),
            Style::new(),
        ));
    }
    if let Some(at) = call.started_at {
        head.push(Span::styled(
            format!("  {} ago", format::ago(now, at)),
            Style::new(),
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
        Span::styled(format!("{name:<10}"), label()),
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
    let text = format!(
        "{} of {} ({})  ",
        format::tokens(context.tokens),
        format::tokens(context.window.unwrap_or_default()),
        format::percent(fill)
    );
    let room = width.saturating_sub(10 + text.chars().count()).min(40);
    let filled = ((fill.clamp(0.0, 1.0) * room as f64).round() as usize).min(room);
    Line::from(vec![
        Span::styled(format!("{:<10}", "context"), label()),
        Span::raw(text),
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
        label(),
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

fn session_lines(app: &App, session: &Session, now: Timestamp, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let status = app::status(session, now);

    lines.extend(
        wrap(app::label(session), width)
            .into_iter()
            .map(|line| line.style(headline())),
    );
    lines.push(Line::from(vec![
        Span::styled(format!("{:<10}", "state"), label()),
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
    // A session the mod does not report on, told from one that is idle.
    if matches!(app.live, LiveStatus::Listening(_)) {
        lines.push(match app.received.sessions.get(&session.id) {
            Some(at) => field(
                "live",
                format!("last event {}", when_said(app.times(), now, *at)),
            ),
            None => Line::from(vec![
                Span::styled(format!("{:<10}", "live"), label()),
                Span::styled("no events from this session", dim()),
            ]),
        });
    }
    lines.push(context_line(session, width));
    if let Some(path) = session.project_path.as_ref().or(session.cwd.as_ref()) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<10}", "cwd"), label()),
            Span::styled(path.display().to_string(), cwd_style()),
        ]));
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
            format::count(session.turns, "interaction"),
            format::count(session.tool_calls, "tool call"),
            format::count(session.subagents.len() as u64, "subagent"),
        ),
    ));
    if let Some(started) = session.started_at {
        lines.push(field("started", format::started(now, started)));
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
            lines.push(Line::styled("rate limits", label()));
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
        Line::styled(call.name.clone(), headline()),
        Line::from(vec![
            Span::styled(format!("{:<10}", "outcome"), label()),
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
        lines.push(field("started", format::started(now, at)));
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
        .map(|line| line.style(headline()))
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
        lines.push(field("started", format::started(now, at)));
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
        label(),
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
                Span::styled(
                    format!(
                        "{mark_at}{:<15}",
                        format::truncate(&format::tool_name(&call.name), 14)
                    ),
                    tool_style(),
                ),
                Span::styled(
                    format!(
                        "{:<room$}",
                        format::truncate(call.summary.as_deref().unwrap_or_default(), room)
                    ),
                    Style::new(),
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
        label(),
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
        "Enter",
        "unfold or fold a subagent in an interaction's tree",
    ),
    ("l / h", "in the trace: the pane on the right, and back"),
    (
        "Esc (list)",
        "clear the search, then the narrowing, step by step",
    ),
    ("/", "search the list"),
    ("n / N", "next / previous match"),
    (
        "t",
        "in the trace: interactions by session, or latest first",
    ),
    ("Enter", "unfold or fold a session in the trace by session"),
    ("T", "times as a date and time, or as how long ago"),
    ("o", "a tool call's whole output, or its excerpt again"),
    ("s", "sort sessions: updated, cost, tokens"),
    ("f", "filter sessions: all, live, open"),
    ("r", "refresh now"),
    ("?", "this help"),
    ("q / Ctrl+c", "quit"),
];

fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    let live = live_lines(app);
    let width = 76.min(area.width);
    let height = ((HELP.len() + live.len()) as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let mut lines: Vec<Line> = HELP
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::styled(format!(" {keys:<18}"), Style::new().fg(Color::Cyan)),
                Span::raw(*what),
            ])
        })
        .collect();
    let shown = usize::from(width).saturating_sub(21);
    lines.extend(live.into_iter().enumerate().map(|(index, text)| {
        let name = if index == 0 { "live events" } else { "" };
        Line::from(vec![
            Span::styled(format!(" {name:<18}"), label()),
            Span::raw(format::truncate(&text, shown)),
        ])
    }));
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

/// The names of fields and the headings inside a pane.
fn label() -> Style {
    Style::new().fg(Color::Cyan)
}

/// The first line of a detail: what it is about.
fn headline() -> Style {
    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
}

/// Where a session runs.
fn cwd_style() -> Style {
    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
}

/// A tool's name.
fn tool_style() -> Style {
    Style::new().fg(Color::Cyan)
}

/// A subagent.
fn agent_style() -> Style {
    Style::new().fg(Color::Magenta)
}

/// Finished, and well.
fn done() -> Style {
    Style::new().fg(Color::Green)
}

/// Not finished yet.
fn running() -> Style {
    Style::new().fg(Color::Yellow)
}

/// How long something took: the slow stand out.
fn took_style(ms: Option<u64>) -> Style {
    match ms {
        Some(60_000..) => Style::new().fg(Color::Red),
        Some(10_000..) => Style::new().fg(Color::Yellow),
        _ => Style::new(),
    }
}

/// What matters least: captions, what is absent or unknown.
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
        assert!(screen.contains("1 interaction, 5 tool calls, 2 subagents"));
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

    /// The part of the screen from one pane's title to the next pane's.
    fn pane<'a>(screen: &'a str, title: &str) -> Vec<&'a str> {
        let top = screen
            .lines()
            .position(|line| line.contains(title))
            .unwrap_or_else(|| panic!("no pane {title}\n{screen}"));
        let lines: Vec<&str> = screen.lines().collect();
        let left = lines[top].find(title).unwrap();
        let chars = |line: &str, to: usize| line[..to.min(line.len())].chars().count();
        let from = chars(lines[top], left);
        // Up to the pane's right edge: the next corner on its title row.
        let width = lines[top][left..]
            .chars()
            .position(|c| c == '┐')
            .map_or(usize::MAX, |at| at + 1);
        lines[top..]
            .iter()
            .take_while(|line| !line.starts_with(' ') || line.trim().is_empty())
            .map(|line| {
                let start = line
                    .char_indices()
                    .nth(from)
                    .map_or(line.len(), |(at, _)| at);
                let end = line
                    .char_indices()
                    .nth(from.saturating_add(width))
                    .map_or(line.len(), |(at, _)| at);
                &line[start..end]
            })
            .take_while(|line| !line.starts_with('└'))
            .collect()
    }

    fn pane_text(screen: &str, title: &str) -> String {
        pane(screen, title).join("\n")
    }

    #[test]
    fn the_trace_page_shows_an_interaction_as_a_waterfall() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        app.enter();
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list

        // Three panes: the interactions, beside them the interaction under the cursor,
        // under them its tree.
        let shown = screen(&mut app, 150, 44, SOON);
        let tops = pane_tops(&shown);
        assert_eq!(tops.len(), 2, "{shown}");
        assert!(shown
            .lines()
            .nth(tops[0])
            .unwrap()
            .contains("┐┌ Interaction "));
        let interactions = pane_text(&shown, "┌ Trace (1 interaction · latest first) of 22222222");
        // The counts have a column each, named in the header.
        let lines = pane(&shown, "┌ Trace (");
        let words = |line: &str| -> Vec<String> {
            let line = line.replace('│', " ");
            line.split_whitespace().map(str::to_owned).collect()
        };
        assert_eq!(
            words(lines[1]),
            ["when", "prompt", "llm", "tools", "took"],
            "{shown}"
        );
        let row = words(lines[2]);
        assert_eq!(row[row.len() - 4..], ["3", "3", "8.0s", "done"], "{shown}");
        assert!(lines[2].contains("Synthetic prompt: run the checks"));
        assert!(interactions.contains("8.0s") && interactions.contains("done"));
        // The list is never unfolded in place.
        assert!(!interactions.contains("cargo test"), "{interactions}");
        assert!(!interactions.contains('█'));

        let interaction = pane_text(&shown, "┌ Interaction ─");
        assert!(interaction.contains("took      8.0s"), "{shown}");
        assert!(interaction.contains("llm       3 requests, 5.5s in all (approximate)"));
        assert!(interaction.contains("tools     3, 3.7s in all"));
        assert!(interaction.contains("subagents 2"));
        assert!(interaction.contains("id        prompt-u1"));

        // The tree: each span with a bar where it was in the interaction.
        let tree = pane(&shown, "┌ Interaction: 3 LLM requests, 3 tool calls");
        let find = |needle: &str| -> &str {
            tree.iter()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no {needle}\n{shown}"))
        };
        let bash = find("cargo test --workspace");
        assert!(bash.contains("Bash") && bash.contains("1.5s") && bash.contains("ok"));
        assert!(find("/work/example/README.md").contains("failed"));
        assert!(find("▸ ↳ Explore").contains("Synthetic agent task"));
        assert!(find("▸ ↳ agent").contains("b2c3d4e5f6071829"));
        let requests: Vec<&&str> = tree
            .iter()
            .filter(|l| l.contains("llm_request  "))
            .collect();
        assert_eq!(requests.len(), 3, "{shown}");
        assert!(requests[0].contains("fable-5-2") && requests[0].contains("tool_use"));
        assert!(requests[2].contains("end_turn"));
        // In the order they happened, left to right.
        assert!(bar_at(requests[0]) < bar_at(bash));
        assert!(bar_at(bash) < bar_at(find("synthetic lookup")));
        assert!(bar_at(requests[1]) < bar_at(requests[2]));
        // The last request ends where the interaction does.
        assert!(requests[2].trim_end_matches('│').ends_with('█'));

        // In the tree, the pane beside the interactions shows the span chosen: a
        // request, a call, a subagent.
        app.enter();
        let request = screen(&mut app, 150, 44, SOON);
        let span = pane_text(&request, "┌ Span ");
        assert!(span.contains("LLM request"), "{request}");
        assert!(span.contains("model     claude-fable-5-2"));
        assert!(span.contains("source    repl_main_thread"));
        assert!(span.contains("stop      tool_use"));
        assert!(span.contains("took      2.4s  approximate"));
        assert!(span.contains("id        msg_1"));
        app.move_by(1);
        let call = screen(&mut app, 150, 44, SOON);
        let span = pane_text(&call, "┌ Span ");
        assert!(span.contains("by        the main thread"), "{call}");
        assert!(span.contains("cargo test --workspace"));
        app.move_by(2);
        app.enter();
        let subagent = screen(&mut app, 150, 44, SOON);
        let tree = pane_text(&subagent, "┌ Interaction: ");
        assert!(tree.contains("▾ ↳ Explore"), "{subagent}");
        assert!(tree.contains("synthetic pattern"));
        assert!(pane_text(&subagent, "┌ Span ").contains("type      Explore"));

        // Without a session entered, rows name their session, and a narrow
        // terminal drops the time axis.
        app.back();
        app.back();
        let all = screen(&mut app, 150, 44, SOON);
        assert!(
            all.contains("Trace (3 interactions · latest first)"),
            "{all}"
        );
        let interactions = pane_text(&all, "┌ Trace (3 interactions");
        assert!(interactions.contains("session") && interactions.contains("example  Synthet"));
        // An interaction of which only the prompt was read.
        assert!(all.contains("nothing was read of this interaction but its prompt"));
        app.move_by(1);
        let narrow = screen(&mut app, 60, 30, SOON);
        assert!(!narrow.contains('█'), "{narrow}");
        assert!(narrow.contains("cargo test"));
    }

    #[test]
    fn the_trace_list_by_session_puts_interactions_under_their_session() {
        let mut app = fixture_app();
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        app.move_by(1);
        // Flat: the session is a column, and beside it what was asked.
        let flat = screen(&mut app, 150, 44, SOON);
        let list = pane_text(&flat, "┌ Trace (3 interactions · latest first)");
        assert!(
            list.contains("session") && list.contains("prompt"),
            "{flat}"
        );
        // (They share the room: both are cut at this width.) The session
        // is where it runs, then its name.
        assert!(list.contains("other.repo  2222"), "{flat}");
        assert!(list.contains("example  Synthet"), "{flat}");
        assert!(list.contains("Synthetic prompt: run the"), "{flat}");
        // The rest of the prompt is not kept.
        assert!(!flat.contains("SYNTHETIC-TEXT"));

        // By session: a row a session, its interactions under it.
        app.toggle_by_session();
        let tree = screen(&mut app, 150, 44, SOON);
        let list = pane(&tree, "┌ Trace (3 interactions · by session)");
        let find = |needle: &str| -> &str {
            list.iter()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no {needle}\n{tree}"))
        };
        // A session's row starts with where it runs.
        let session = find("▾ other.repo");
        assert!(session.contains("22222222") && session.contains("active"));
        assert!(find("Synthetic session title").contains("▾ example"));
        // An interaction is under its session, with its counts; the
        // session's row has the totals.
        let under = find("8.0s").replace('│', " ");
        let under: Vec<&str> = under.split_whitespace().collect();
        assert_eq!(
            under,
            [
                "20s",
                "└",
                "Synthetic",
                "prompt:",
                "run",
                "the",
                "checks",
                "3",
                "3",
                "8.0s",
                "done"
            ]
        );
        assert!(
            session.split_whitespace().any(|word| word == "3"),
            "{session}"
        );
        assert!(
            list[1].contains("session / prompt") && list[1].contains("tools"),
            "{tree}"
        );
        // The panes still show the interaction the cursor was on.
        assert!(tree.contains("┌ Interaction: 3 LLM requests, 3 tool calls"));

        // On a session's row the panes are about the session, not about
        // one of its interactions: the session beside the list, no tree.
        app.leave_detail();
        let on_session = screen(&mut app, 150, 44, SOON);
        assert!(
            !on_session.contains("┌ Interaction: 3 LLM requests"),
            "{on_session}"
        );
        assert!(on_session.contains("a session: the tree is of one of its interactions"));
        assert!(!on_session.contains("took      8.0s"), "{on_session}");
        assert!(pane_text(&on_session, "┌ Session ").contains("22222222"));

        // Folded, a session is one row, and the panes say the same.
        app.enter();
        let folded = screen(&mut app, 150, 44, SOON);
        let list = pane_text(&folded, "┌ Trace (3 interactions · by session)");
        assert!(list.contains("▸ other.repo  22222222"), "{folded}");
        assert!(!list.contains("8.0s"));
        assert!(folded.contains("a session: the tree is of one of its interactions"));
        assert!(pane_text(&folded, "┌ Session ").contains("22222222"));
    }

    #[test]
    fn the_pane_beside_the_trace_list_takes_the_focus_from_either_side() {
        let mut app = fixture_app();
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        app.move_by(1);
        // From the list: it shows the interaction, and the tree has no
        // cursor.
        app.right();
        let beside = screen(&mut app, 150, 20, SOON);
        assert!(beside.contains("┌ Interaction ["), "{beside}");
        assert!(app.viewport.call_lines > app.viewport.call_rows);
        app.move_last();
        let scrolled = screen(&mut app, 150, 20, SOON);
        assert!(pane_text(&scrolled, "┌ Interaction [").contains("id        prompt-u1"));
        // From the tree: the span.
        app.leave_detail();
        app.enter();
        app.move_by(1);
        app.right();
        let span = screen(&mut app, 150, 44, SOON);
        assert!(
            pane_text(&span, "┌ Span ").contains("cargo test --workspace"),
            "{span}"
        );
    }

    #[test]
    fn an_interaction_without_its_prompt_is_told_by_the_tools_it_used() {
        use crate::app::tests::{append_call, scratch_projects};
        let projects = scratch_projects("trace-tools");
        for (id, name) in [
            ("toolu_n1", "Bash"),
            ("toolu_n2", "Read"),
            ("toolu_n3", "Bash"),
        ] {
            append_call(&projects, id, 1, name, r#"{"command":"ls"}"#);
        }
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
            time: crate::app::TimeStyle::default(),
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
        });
        let session = app.store.get(WITH_SUBAGENTS).unwrap();
        let known = session.trace.back().unwrap();
        assert_eq!(
            interaction_text(known).0,
            "Synthetic prompt: run the checks"
        );
        // The same interaction, had its prompt not been read.
        let mut unknown = known.clone();
        unknown.prompt = None;
        let (text, style) = interaction_text(&unknown);
        assert_eq!(text, "Bash×3 Read×2 example/lookup");
        assert_eq!(style, tool_style());

        // A search finds an interaction by what was asked.
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        app.start_search();
        app.typing = Some("run the checks".to_owned());
        app.submit_search();
        let now = Timestamp::parse(LATER).unwrap();
        assert_eq!(app.matches(Page::Trace, now).len(), 3);

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn the_tree_follows_its_cursor_and_keeps_its_place() {
        use crate::app::tests::{append_agent_call, scratch_projects};
        let projects = scratch_projects("trace-scroll");
        for index in 2..40 {
            let input = format!(r#"{{"command":"echo call-{index}"}}"#);
            append_agent_call(&projects, &format!("toolu_s{index:02}"), index, &input);
        }
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
            time: crate::app::TimeStyle::default(),
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
        });
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        app.move_by(1);
        app.enter();
        app.move_by(3);
        app.enter();
        // The subagent's calls are far more than the pane shows.
        let top = screen(&mut app, 150, 40, LATER);
        assert!(
            pane_text(&top, "┌ Interaction: ").contains("synthetic pattern"),
            "{top}"
        );
        assert!(!pane_text(&top, "┌ Interaction: ").contains("call-39"));
        app.move_last();
        let bottom = screen(&mut app, 150, 40, LATER);
        assert!(
            !pane_text(&bottom, "┌ Interaction: ").contains("cargo test"),
            "{bottom}"
        );
        assert!(bottom.contains("b2c3d4e5f6071829"));
        assert!(app.offsets[TREE] > 0);
        // Half a page at a time, by what the tree shows.
        assert!(app.viewport.detail_rows > 4);
        assert_eq!(app.half_page() as usize, app.viewport.detail_rows / 2);
        // Another interaction starts at its top.
        app.back();
        app.move_by(1);
        screen(&mut app, 150, 40, LATER);
        assert_eq!((app.span_selected, app.offsets[TREE]), (0, 0));

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn an_interaction_without_an_end_runs_or_was_cut_off() {
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
            time: crate::app::TimeStyle::default(),
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
        });
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
                                // Under the interaction of a session that is a day later.
        app.move_by(1);
        assert_eq!(
            app.selected_interaction().unwrap().interaction.id,
            "prompt-new"
        );

        // Its session was active a moment ago: everything reaches to now.
        let running = screen(&mut app, 150, 44, "2026-11-02T11:01:30.000Z");
        let interaction = row(&running, "1m30s");
        assert!(
            interaction.contains("running") && interaction.contains("1m30s"),
            "{running}"
        );
        let call = row(&running, "sleep 600");
        assert!(
            call.contains("running") && call.contains("30.0s"),
            "{running}"
        );
        assert!(call.trim_end_matches('│').ends_with('█'));
        let detail = pane_text(&running, "┌ Interaction ─");
        assert!(detail.contains("state     running"), "{running}");
        assert!(detail.contains("so far, by the records"));

        // Long after: all that is known is that it has no end.
        let later = screen(&mut app, 150, 44, LATER);
        let interaction = row(&later, "1m00s");
        assert!(
            interaction.contains("no end") && interaction.contains("1m00s"),
            "{later}"
        );
        assert!(!row(&later, "sleep 600")
            .trim_end_matches('│')
            .ends_with("██"));
        assert!(later.contains("no end recorded"));

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn kinds_states_and_labels_are_told_apart_by_colour() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        app.enter();
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        let now = Timestamp::parse(SOON).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        // The colour of the first cell of `word` on the screen, or of the
        // one `skip` cells into it.
        let colour_at = |word: &str, skip: u16| -> Color {
            for y in 0..44u16 {
                let line: Vec<&str> = (0..150u16).map(|x| buffer[(x, y)].symbol()).collect();
                let text = line.concat();
                if let Some(at) = text.find(word) {
                    let x = text[..at].chars().count() as u16;
                    return buffer[(x + skip, y)].fg;
                }
            }
            panic!("no {word} on the screen");
        };
        let colour = |word: &str| colour_at(word, 0);
        // What a row is.
        assert_eq!(colour("llm_request"), Color::LightBlue);
        assert_eq!(colour("Bash"), Color::Cyan);
        assert_eq!(colour("↳ Explore"), Color::Magenta);
        // What a call is about is not greyed out.
        assert_eq!(colour("cargo test --workspace"), Color::Reset);
        // How it ended.
        assert_eq!(colour_at("s  ok ", 3), Color::Green);
        assert_eq!(colour("failed"), Color::Red);
        assert_eq!(colour("end_turn"), Color::Green);
        assert_eq!(colour("tool_use"), Color::DarkGray);
        assert_eq!(colour("running"), Color::Yellow);
        // The detail: its headline, its field names, its values.
        assert_eq!(colour("Interaction  "), Color::Yellow);
        assert_eq!(colour("started   "), Color::Cyan);
        assert_eq!(colour("prompt-u1"), Color::Reset);

        // The slow stand out.
        assert_eq!(took_style(Some(9_999)).fg, None);
        assert_eq!(took_style(Some(10_000)).fg, Some(Color::Yellow));
        assert_eq!(took_style(Some(60_000)).fg, Some(Color::Red));
        assert_eq!(took_style(None).fg, None);
    }

    /// An app on the fixture session whose calls return every kind of
    /// output, on the Tool calls page with the cursor on one call.
    fn on_call(id: &str) -> App {
        let mut app = App::new(Config {
            projects_dir: crate::app::tests::fixtures()
                .join(iktinos::record::KNOWN_CLAUDE_CODE_VERSION)
                .join("output"),
            live: false,
            socket_dir: None,
            time: TimeStyle::default(),
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
        });
        app.go_to(Page::Tools);
        let at = app
            .tools()
            .iter()
            .position(|(_, call)| call.id == id)
            .unwrap();
        app.move_by(at as isize);
        app
    }

    #[test]
    fn a_tool_calls_detail_shows_what_it_returned() {
        let detail = |id: &str| -> String {
            let screen = screen(&mut on_call(id), 150, 60, LATER);
            pane_text(&screen, "┌ Tool call ")
        };
        // A success: under the command, the output.
        let ok = detail("toolu_o1");
        assert!(ok.contains("output    2 lines, 36 characters"), "{ok}");
        assert!(ok.contains("Synthetic output: 3 passed") && ok.contains("all green"));
        assert!(!ok.contains("stderr") && !ok.contains("(o:"));
        // An error, stderr, an interruption, a block that is not text.
        assert!(detail("toolu_o2").contains("Error: synthetic failure, no such file"));
        let warned = detail("toolu_o3");
        assert!(warned.contains("stderr"), "{warned}");
        assert!(warned.contains("synthetic warning: deprecated flag"));
        assert!(warned.contains("second line of stderr"));
        assert!(detail("toolu_o7").contains("interrupted"));
        let shot = detail("toolu_o5");
        assert!(shot.contains("Synthetic caption of a screenshot"), "{shot}");
        assert!(shot.contains("and 1 block that is not text"));

        // Over what is kept: the start, and how much there is.
        let long = detail("toolu_o4");
        assert!(
            long.contains(
                "output    201 lines, 6029 characters; the start is shown  (o: all of it)"
            ),
            "{long}"
        );
        assert!(long.contains("line 0001 of synthetic output"));
        assert!(!long.contains("SYNTHETIC-TEXT"));
    }

    #[test]
    fn o_reads_the_whole_output_and_shows_it_in_place_of_the_excerpt() {
        let mut app = on_call("toolu_o4");
        app.load_output();
        let whole = app.whole.clone().unwrap();
        assert_eq!(whole.call, "toolu_o4");
        let text = whole.output.unwrap().unwrap().text;
        assert!(text.ends_with("SYNTHETIC-TEXT past the limit"));
        // The detail has it all: its last line is reached by scrolling.
        app.focus_detail();
        screen(&mut app, 150, 40, LATER);
        app.move_last();
        let end = screen(&mut app, 150, 40, LATER);
        let detail = pane_text(&end, "┌ Tool call ");
        assert!(detail.contains("line 0200 of synthetic output"), "{end}");
        assert!(detail.contains("past the limit"));
        app.move_first();
        let top = screen(&mut app, 150, 60, LATER);
        assert!(
            top.contains("output    all of it: 201 lines, 6029 characters  (o: the excerpt)"),
            "{top}"
        );
        // Again: the excerpt.
        app.load_output();
        assert_eq!(app.whole, None);
        assert!(screen(&mut app, 150, 60, LATER).contains("the start is shown"));

        // An output that was moved to a file is read from the file.
        let mut app = on_call("toolu_o6");
        let before = screen(&mut app, 150, 60, LATER);
        assert!(before.contains("Synthetic preview of an output moved to a file"));
        app.load_output();
        let moved = screen(&mut app, 150, 60, LATER);
        assert!(moved.contains("output    all of it: 400 lines"), "{moved}");
        assert!(moved.contains("tool-results/toolu_o6.txt"));
        assert!(moved.contains("whole line 0001 of the moved synthetic output"));
        assert!(!moved.contains("Synthetic preview"));
        // Another call's output replaces it: one is held at a time.
        app.leave_detail();
        app.move_by(1);
        app.load_output();
        assert_ne!(app.whole.as_ref().unwrap().call, "toolu_o6");

        // Nothing to read where no call is shown.
        app.go_to(Page::Usage);
        app.whole = None;
        app.load_output();
        assert_eq!(app.whole, None);
    }

    #[test]
    fn the_output_is_beside_a_subagents_call_and_a_traces_span_too() {
        let mut app = fixture_app();
        // A subagent's call, in the pane beside the subagent.
        app.go_to(Page::Subagents);
        let subagents = screen(&mut app, 150, 44, LATER);
        assert!(
            pane_text(&subagents, "┌ Tool call ").contains("Synthetic output of toolu_s1"),
            "{subagents}"
        );
        app.focus_detail();
        app.load_output();
        assert_eq!(app.whole.as_ref().unwrap().call, "toolu_s1");
        assert!(screen(&mut app, 150, 44, LATER).contains("output    all of it: 1 line"));

        // A span of the trace, in the pane beside the list.
        app.go_to(Page::Trace);
        app.by_session = false;
        app.move_by(1);
        app.load_output();
        assert_eq!(
            app.whole.as_ref().unwrap().call,
            "toolu_s1",
            "no span is chosen yet"
        );
        app.enter();
        app.move_by(1);
        let span = screen(&mut app, 150, 44, LATER);
        assert!(
            pane_text(&span, "┌ Span ").contains("Synthetic output of toolu_01"),
            "{span}"
        );
        // Its whole output was moved to a file.
        app.load_output();
        let whole = screen(&mut app, 150, 44, LATER);
        assert!(
            pane_text(&whole, "┌ Span ").contains("Synthetic whole output of toolu_01"),
            "{whole}"
        );
        // An LLM request has no output to read.
        app.move_by(-1);
        app.load_output();
        assert_eq!(app.whole.as_ref().unwrap().call, "toolu_01");
    }

    #[test]
    fn times_are_a_date_and_time_or_how_long_ago() {
        let with = |ago_within: std::time::Duration| {
            let mut app = App::new(Config {
                projects_dir: crate::app::tests::fixtures()
                    .join(iktinos::record::KNOWN_CLAUDE_CODE_VERSION)
                    .join("projects"),
                live: false,
                socket_dir: None,
                time: TimeStyle::Clock,
                ago_within,
            });
            app.go_to(Page::Tools);
            app
        };
        let bash = |app: &mut App, now: &str| {
            let screen = screen(app, 150, 30, now);
            row(&screen, "cargo test --workspace").to_owned()
        };
        // What is new is said as how long ago: within the hour, unless
        // told otherwise.
        let mut app = fixture_app();
        app.go_to(Page::Tools);
        assert!(bash(&mut app, SOON).contains(" 18s "));
        assert!(bash(&mut app, "2026-11-02T10:59:00.000Z").contains(" 58m "));
        // After that, the time; another day, the date too. (Under test the
        // clock is UTC.)
        assert!(bash(&mut app, "2026-11-02T11:00:02.000Z").contains("10:00:02"));
        assert!(bash(&mut app, LATER).contains("11-02 10:00"));
        // A detail says both, always.
        assert!(screen(&mut app, 150, 30, LATER).contains("started   11-02 10:00 (1d ago)"));

        // Never "ago", or for longer.
        let mut never = with(std::time::Duration::ZERO);
        assert!(bash(&mut never, SOON).contains("10:00:02"));
        let mut days = with(std::time::Duration::from_secs(3 * 86_400));
        assert!(bash(&mut days, LATER).contains(" 1d "));

        // `T`: how long ago for everything, and back.
        app.toggle_time();
        let relative = bash(&mut app, LATER);
        assert!(
            relative.contains(" 1d ") && !relative.contains("11-02"),
            "{relative}"
        );
        // The other lists follow. (A detail pane says both, always.)
        for (page, title) in [
            (Page::Subagents, "┌ Subagents ("),
            (Page::Trace, "┌ Trace ("),
        ] {
            app.go_to(page);
            let list = |app: &mut App| pane_text(&screen(app, 150, 30, LATER), title);
            assert!(!list(&mut app).contains("11-02 10:00"));
            app.toggle_time();
            assert!(list(&mut app).contains("11-02 10:00"));
            app.toggle_time();
        }
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
            time: crate::app::TimeStyle::default(),
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
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

    /// Posts a body to the viewer's socket and waits for the answer.
    #[cfg(unix)]
    fn post(socket: &std::path::Path, body: &str) -> String {
        use std::io::{Read, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        write!(
            stream,
            "POST /events HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[cfg(unix)]
    #[test]
    fn the_header_says_what_the_socket_has_received() {
        use crate::app::tests::{fixtures, WITH_SUBAGENTS};

        let socket_dir = std::env::temp_dir().join(format!("ccs-tui-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&socket_dir);
        let mut app = App::new(Config {
            projects_dir: fixtures()
                .join(iktinos::record::KNOWN_CLAUDE_CODE_VERSION)
                .join("projects"),
            live: true,
            socket_dir: Some(socket_dir.clone()),
            time: TimeStyle::Clock,
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
        });
        let LiveStatus::Listening(socket) = app.live.clone() else {
            panic!("{:?}", app.live)
        };
        select(&mut app, WITH_SUBAGENTS);

        // Listening is not receiving.
        let quiet = screen(&mut app, 150, 44, SOON);
        let header = quiet.lines().next().unwrap();
        assert!(header.ends_with("live on, no events yet"), "{header}");
        assert!(quiet.contains("no events from this session"), "{quiet}");

        // Two events of this session, one of a type nobody knows, and a
        // request that is not an event.
        for name in ["turn-start", "tool-check-ask", "unknown-type"] {
            let body =
                std::fs::read_to_string(fixtures().join("events").join(format!("{name}.json")))
                    .unwrap();
            assert!(post(&socket, &body).starts_with("HTTP/1.1 200"), "{name}");
        }
        assert!(post(&socket, "{}").starts_with("HTTP/1.1 400"));
        app.refresh_at(Timestamp::parse(SOON).unwrap());
        select(&mut app, WITH_SUBAGENTS);

        let now = screen(&mut app, 150, 44, SOON);
        let header = now.lines().next().unwrap();
        assert!(header.ends_with("live 3 events, last now"), "{header}");
        assert!(now.contains("last event now"), "{now}");

        let soon = screen(&mut app, 150, 44, "2026-11-02T10:00:50.000Z");
        let header = soon.lines().next().unwrap();
        assert!(header.ends_with("live 3 events, last 30s ago"), "{header}");

        // Past what is said as how long ago, the time itself.
        let later = screen(&mut app, 150, 44, LATER);
        let header = later.lines().next().unwrap();
        assert!(
            header.ends_with("live 3 events, last at 11-02 10:00"),
            "{header}"
        );
        assert!(later.contains("last event at 11-02 10:00"), "{later}");

        // The help has the rest: where it listens, and what did not fit.
        app.help = true;
        let help = screen(&mut app, 150, 44, LATER);
        assert!(help.contains("live events"), "{help}");
        assert!(
            help.contains("3 events, 1 of an unknown type, 1 request rejected"),
            "{help}"
        );

        drop(app);
        let _ = std::fs::remove_dir_all(socket_dir);
    }

    #[test]
    fn a_viewer_that_does_not_listen_says_nothing_about_events() {
        let mut app = fixture_app();
        select(&mut app, WITH_SUBAGENTS);
        app.help = true;
        let screen = screen(&mut app, 150, 44, SOON);
        assert!(!screen.contains("live events"), "{screen}");
        assert!(!screen.contains("this session"), "{screen}");
    }

    /// The fixture sessions after these event samples, in this order.
    #[cfg(unix)]
    fn with_events(names: &[&str]) -> App {
        use crate::app::tests::fixtures;
        let mut app = fixture_app();
        for name in names {
            let body =
                std::fs::read(fixtures().join("events").join(format!("{name}.json"))).unwrap();
            let event = iktinos_live::event::Event::parse(&body).unwrap();
            iktinos_live::merge::apply_event(&mut app.store, &event);
        }
        app.arrange(Timestamp::parse(SOON).unwrap());
        select(&mut app, WITH_SUBAGENTS);
        app
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_awaiting_approval_asks_for_attention() {
        let mut app = with_events(&["turn-start", "tool-check-ask"]);
        let shown = screen(&mut app, 150, 44, SOON);
        let header = shown.lines().next().unwrap();
        assert!(header.contains("1 need approval"), "{header}");
        assert!(row(&shown, "other.repo").contains("approve Write?"));
        assert!(shown.contains("state     approve Write?"), "{shown}");

        // In the colour of what needs someone.
        let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
        let now = Timestamp::parse(SOON).unwrap();
        terminal.draw(|frame| draw(frame, &mut app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<&str> = shown.lines().collect();
        let y = lines.iter().position(|l| l.contains("other.repo")).unwrap();
        let x = lines[y][..lines[y].find("approve").unwrap()]
            .chars()
            .count();
        let cell = &buffer[(x as u16, y as u16)];
        assert_eq!(cell.fg, Color::Yellow);
        assert!(cell.modifier.contains(Modifier::BOLD));

        // The trace's row for the session has room for a word.
        app.go_to(Page::Trace);
        let trace = screen(&mut app, 150, 30, SOON);
        assert!(row(&trace, "▾ other.repo").contains("approve?"), "{trace}");
    }

    #[cfg(unix)]
    #[test]
    fn a_running_tool_is_named_with_what_it_runs() {
        let mut app = with_events(&["turn-start", "tool-check-allow", "tool-call-start"]);
        let shown = screen(&mut app, 150, 44, SOON);
        assert!(shown.lines().next().unwrap().contains("2 busy"), "{shown}");
        assert!(row(&shown, "other.repo").contains("running Bash"));
        let detail = shown.clone();
        let state = detail
            .lines()
            .position(|line| line.contains("state     running Bash"))
            .unwrap_or_else(|| panic!("{shown}"));
        // What it runs, under the state.
        assert!(
            detail
                .lines()
                .nth(state + 1)
                .unwrap()
                .contains("cargo test --workspace"),
            "{shown}"
        );

        // The call ended: the session works on, with no tool to name.
        let mut app = with_events(&[
            "turn-start",
            "tool-check-allow",
            "tool-call-start",
            "tool-call-end",
        ]);
        let shown = screen(&mut app, 150, 44, SOON);
        assert!(row(&shown, "other.repo").contains("working"), "{shown}");
        assert!(!shown.contains("cargo test --workspace"), "{shown}");
    }

    #[cfg(unix)]
    #[test]
    fn rate_limits_are_listed_with_how_much_is_used() {
        let mut app = with_events(&["turn-start", "session-measure"]);
        let shown = screen(&mut app, 150, 60, SOON);
        let detail = shown.clone();
        assert!(detail.contains("rate limits"), "{shown}");
        let five = row(&detail, "five_hour");
        assert!(five.contains("42%"), "{shown}");
        assert!(row(&detail, "seven_day").contains("7%"), "{shown}");
        // The context is measured against the window in effect.
        assert!(detail.contains("6.7k of 200.0k"), "{shown}");
    }

    #[cfg(unix)]
    #[test]
    fn waiting_working_and_ended_sessions_are_told_apart() {
        let state = |names: &[&str]| {
            let mut app = with_events(names);
            let shown = screen(&mut app, 150, 44, SOON);
            let header = shown.lines().next().unwrap().to_owned();
            let listed = row(&shown, "other.repo").to_owned();
            app.go_to(Page::Trace);
            let trace = screen(&mut app, 150, 30, SOON);
            (header, listed, row(&trace, "▾ other.repo").to_owned())
        };

        let (header, listed, trace) = state(&["turn-start"]);
        assert!(header.contains("2 busy"), "{header}");
        assert!(listed.contains("working"), "{listed}");
        // Live events say the session works: so does its row in the trace,
        // whatever the transcript's last interaction came to.
        assert!(trace.contains("working"), "{trace}");

        let (header, listed, trace) = state(&["turn-start", "turn-complete"]);
        assert!(header.contains("1 busy  1 waiting"), "{header}");
        assert!(listed.contains("waiting"), "{listed}");
        assert!(trace.contains("waiting"), "{trace}");

        let (header, listed, trace) = state(&["turn-start", "session-end"]);
        assert!(header.contains("1 busy  live off"), "{header}");
        assert!(listed.contains("exited"), "{listed}");
        assert!(trace.contains("exited"), "{trace}");
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
        assert!(screen.contains("unfold or fold a subagent"));
        assert!(screen.contains("interactions by session, or latest first"));
        assert!(screen.contains("a tool call's whole output"));
        assert!(screen.contains("the pane on the right, and back"));
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
            time: crate::app::TimeStyle::default(),
            ago_within: crate::app::DEFAULT_AGO_WITHIN,
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
        println!("{}", screen(&mut app, 150, 36, SOON));
        app.back();
        println!("{}", screen(&mut app, 150, 20, SOON));
        app.toggle_by_session();
        println!("{}", screen(&mut app, 150, 20, SOON));
        app.toggle_by_session();
        app.go_to(Page::Sessions);
        app.enter();
        app.go_to(Page::Trace);
        app.enter();
        app.move_by(3);
        app.enter();
        app.move_by(2);
        println!("{}", screen(&mut app, 150, 36, SOON));
        app.back();
        app.back();
        println!("{}", screen(&mut app, 110, 30, SOON));
    }

    #[test]
    fn no_unmodelled_text_reaches_the_screen() {
        let mut app = fixture_app();
        for page in Page::ALL {
            app.go_to(page);
            if page == Page::Trace {
                // Every interaction's tree, with every subagent unfolded.
                for interaction in 0..4 {
                    app.in_detail = false;
                    app.move_first();
                    app.move_by(interaction);
                    app.focus_detail();
                    let agents: Vec<(String, String)> = app
                        .store
                        .sessions()
                        .flat_map(|s| s.subagents.keys().map(|a| (s.id.clone(), a.clone())))
                        .collect();
                    app.open_agents.extend(agents);
                    for _ in 0..12 {
                        let screen = screen(&mut app, 160, 50, LATER);
                        assert!(!screen.contains("SYNTHETIC-TEXT"), "{screen}");
                        app.move_by(1);
                    }
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

//! The viewer's state: a `SessionStore` kept current from the transcripts
//! and, when the bridge mod is installed, from live events.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iktinos::discover::{discover, SessionFiles};
use iktinos::record::{Record, Timestamp};
use iktinos::session::{Activity, LiveState, Session, SessionStore, Subagent, ToolCall};
use iktinos::tail::Tail;
use iktinos::trace::{Interaction, Item, Span, SpanKind};

/// How recent a record makes a session "active" when no live event says more.
const ACTIVE_WITHIN: Duration = Duration::from_secs(60);

/// What the viewer was asked to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub projects_dir: PathBuf,
    /// Listen for live events.
    pub live: bool,
    /// Where to listen; the shared default when `None`.
    pub socket_dir: Option<PathBuf>,
    /// How the lists say when something happened, to begin with.
    pub time: TimeStyle,
    /// With [`TimeStyle::Clock`], what is newer than this is still shown as
    /// how long ago it was.
    pub ago_within: Duration,
}

/// How new something is shown as "ago" unless told otherwise.
pub const DEFAULT_AGO_WITHIN: Duration = Duration::from_secs(3600);

/// A length of time as an option spells it: `90s`, `30m`, `1h`, `2d`, or a
/// number of seconds.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let (number, unit) = match text.char_indices().last()? {
        (at, 's') => (&text[..at], 1),
        (at, 'm') => (&text[..at], 60),
        (at, 'h') => (&text[..at], 3_600),
        (at, 'd') => (&text[..at], 86_400),
        _ => (text, 1),
    };
    let seconds = number.parse::<u64>().ok()?.checked_mul(unit)?;
    Some(Duration::from_secs(seconds))
}

/// How times are shown now: the style, and how new is still "ago".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Times {
    pub style: TimeStyle,
    pub ago_within: Duration,
}

/// How the lists say when something happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimeStyle {
    /// The local date and time, `10:00:08` or `11-02 10:00`, except for
    /// what is new enough to be said as how long ago.
    #[default]
    Clock,
    /// How long ago: `12s`, `5m`.
    Relative,
}

impl TimeStyle {
    /// `clock` or `relative`, as the `--time` option spells them.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "clock" => Some(Self::Clock),
            "relative" => Some(Self::Relative),
            _ => None,
        }
    }
}

/// Whether live events can arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveStatus {
    /// Not asked for, or not available on this platform.
    Off,
    /// Listening on this socket.
    Listening(PathBuf),
    /// Asked for, but the socket could not be opened.
    Failed(String),
}

/// One line of what a session is doing, and how much attention it wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub kind: StatusKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// Working: an interaction in progress, a tool running, or a fresh record.
    Busy,
    /// Blocked on a person: a permission prompt.
    NeedsApproval,
    /// At the prompt, waiting for its user.
    Waiting,
    /// Nothing recent.
    Idle,
    /// The process exited.
    Exited,
}

/// The pages, in header order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Sessions,
    Tools,
    Subagents,
    Trace,
    Usage,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::Sessions,
        Page::Tools,
        Page::Subagents,
        Page::Trace,
        Page::Usage,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Sessions => "Sessions",
            Page::Tools => "Tool calls",
            Page::Subagents => "Subagents",
            Page::Trace => "Trace",
            Page::Usage => "Usage",
        }
    }

    /// Whether the page is a list over a detail pane.
    pub fn has_list(self) -> bool {
        self != Page::Usage
    }

    /// Which of the lists' scroll positions is the page's.
    pub fn list_index(self) -> usize {
        match self {
            Page::Sessions | Page::Usage => 0,
            Page::Tools => 1,
            Page::Subagents => 2,
            Page::Trace => 3,
        }
    }
}

/// How the session list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Updated,
    Cost,
    Tokens,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Updated => "updated",
            Sort::Cost => "cost",
            Sort::Tokens => "tokens",
        }
    }
}

/// Which sessions the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    /// Working, or wanting someone's attention.
    Live,
    /// Everything whose process has not exited.
    Open,
}

impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Live => "live",
            Filter::Open => "open",
        }
    }

    fn admits(self, kind: StatusKind) -> bool {
        match self {
            Filter::All => true,
            Filter::Live => matches!(
                kind,
                StatusKind::Busy | StatusKind::NeedsApproval | StatusKind::Waiting
            ),
            Filter::Open => kind != StatusKind::Exited,
        }
    }
}

/// A search within one page's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub page: Page,
    /// Lower-cased: matching ignores case.
    pub query: String,
}

/// What the last draw measured, for keys that move by what is visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Viewport {
    /// Rows the list pane shows.
    pub list_rows: usize,
    /// Rows the detail pane shows.
    pub detail_rows: usize,
    /// Lines the detail pane holds.
    pub detail_lines: usize,
    /// Rows and lines of the tool call pane beside a subagent's detail.
    pub call_rows: usize,
    pub call_lines: usize,
}

pub struct App {
    config: Config,
    pub store: SessionStore,
    /// A tail per transcript, with the session it belongs to.
    tails: BTreeMap<PathBuf, (String, Tail)>,
    pub page: Page,
    /// The detail pane has the focus, not the list above it.
    pub in_detail: bool,
    pub sort: Sort,
    pub filter: Filter,
    pub time: TimeStyle,
    /// The ids of the sessions the Sessions page lists, in display order.
    pub order: Vec<String>,
    /// The session the Tool calls and Subagents pages are narrowed to.
    pub scope: Option<String>,
    /// Within that session, the subagent the Tool calls page is narrowed to.
    pub agent_scope: Option<String>,
    pub selected: usize,
    pub tool_selected: usize,
    pub subagent_selected: usize,
    pub detail_scroll: usize,
    /// The tool call under the cursor of a subagent's detail pane, which
    /// lists the subagent's calls.
    pub trace_selected: usize,
    /// The Trace page lists its interactions under their sessions, as it
    /// does to begin with, instead of all together, the latest first.
    pub by_session: bool,
    /// The sessions folded in that list, by id.
    pub closed_sessions: HashSet<String>,
    /// The row under the cursor of the tree the Trace page shows for its
    /// interaction.
    pub span_selected: usize,
    /// The subagents unfolded in that tree, by session and agent id.
    pub open_agents: HashSet<(String, String)>,
    pub call_selected: usize,
    /// The pane beside a subagent's detail, which shows that call, has the
    /// focus. Only ever set while `in_detail` is.
    pub call_focus: bool,
    pub call_scroll: usize,
    /// The Trace page's pane beside its list was entered from the list, not
    /// from the tree: it shows the interaction, and `h` goes back to the
    /// list.
    pub from_list: bool,
    /// The first visible row of each list ([`Page::list_index`]; the last
    /// is the Trace page's tree), kept between draws so a list scrolls only
    /// when the cursor leaves it.
    pub offsets: [usize; 5],
    pub search: Option<Search>,
    /// The search being typed, after `/`.
    pub typing: Option<String>,
    pub help: bool,
    pub viewport: Viewport,
    /// Something the user should know: a missing directory, a failed socket.
    pub notice: Option<String>,
    pub live: LiveStatus,
    #[cfg(unix)]
    listener: Option<live::Listener>,
}

/// How many recent tool calls per session the viewer keeps.
const RECENT_TOOLS: usize = 200;

/// How many rows the Tool calls page lists at most.
const TOOL_ROWS: usize = 500;

/// How many interactions the Trace page lists at most.
const INTERACTION_ROWS: usize = 200;

/// Which of the lists' scroll positions is the Trace page's tree's.
pub const TREE: usize = 4;

/// One row of the Trace page's list: a session, or under it one of its
/// interactions.
#[derive(Debug, Clone, Copy)]
pub struct ListRow<'a> {
    pub session: &'a Session,
    /// `None` on the session's own row.
    pub interaction: Option<InteractionRow<'a>>,
    /// On a session's row: whether its interactions are listed under it.
    pub open: bool,
}

impl ListRow<'_> {
    /// The text the row is searched by.
    fn text(&self) -> String {
        match &self.interaction {
            Some(interaction) => interaction.text(),
            None => format!(
                "{} {}",
                label(self.session),
                project(self.session).unwrap_or_default()
            ),
        }
    }
}

/// One interaction in the Trace page's list.
#[derive(Debug, Clone, Copy)]
pub struct InteractionRow<'a> {
    pub session: &'a Session,
    pub interaction: &'a Interaction,
    /// The last moment known of the interaction: its end, or the latest of what
    /// was read of its main thread.
    pub end: Option<Timestamp>,
}

impl InteractionRow<'_> {
    /// The text the interaction is searched by: its session, what was asked
    /// and what its main thread called.
    fn text(&self) -> String {
        let mut text = format!(
            "interaction {} {}",
            label(self.session),
            self.interaction.prompt.as_deref().unwrap_or_default()
        );
        for span in &self.interaction.thread.spans {
            if let SpanKind::Tool(call) = &span.kind {
                text.push(' ');
                text.push_str(&call.name);
                text.push(' ');
                text.push_str(call.summary.as_deref().unwrap_or_default());
            }
        }
        text
    }
}

/// One row of the tree under the Trace page's list: a span or a subagent of
/// the interaction under the cursor.
#[derive(Debug, Clone, Copy)]
pub struct TraceRow<'a> {
    pub session: &'a Session,
    pub interaction: &'a Interaction,
    /// The last moment known of the interaction, subagents included.
    pub interaction_end: Option<Timestamp>,
    /// 0 for what hangs directly under the interaction.
    pub depth: usize,
    pub item: TraceItem<'a>,
    /// Whether a row that folds (a subagent) is unfolded. `None` for one
    /// that does not.
    pub open: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub enum TraceItem<'a> {
    Span(&'a Span),
    Subagent(&'a Subagent),
}

impl TraceRow<'_> {
    /// What names the row across refreshes.
    pub fn key(&self) -> String {
        match self.item {
            TraceItem::Span(span) => match &span.kind {
                SpanKind::Tool(call) => format!("t:{}", call.id),
                SpanKind::LlmRequest(request) => format!(
                    "r:{}:{}",
                    request.message_id.as_deref().unwrap_or_default(),
                    span.started_at.map_or(0, |at| at.as_millis())
                ),
                _ => String::new(),
            },
            TraceItem::Subagent(subagent) => format!("a:{}", subagent.agent_id),
        }
    }
}

impl App {
    pub fn new(config: Config) -> Self {
        let mut app = Self {
            store: SessionStore::new().with_recent_tools(RECENT_TOOLS),
            tails: BTreeMap::new(),
            page: Page::Sessions,
            in_detail: false,
            sort: Sort::Updated,
            filter: Filter::All,
            time: config.time,
            order: Vec::new(),
            scope: None,
            agent_scope: None,
            selected: 0,
            tool_selected: 0,
            subagent_selected: 0,
            detail_scroll: 0,
            trace_selected: 0,
            by_session: true,
            closed_sessions: HashSet::new(),
            span_selected: 0,
            open_agents: HashSet::new(),
            call_selected: 0,
            call_focus: false,
            call_scroll: 0,
            from_list: false,
            offsets: [0; 5],
            search: None,
            typing: None,
            help: false,
            viewport: Viewport::default(),
            notice: None,
            live: LiveStatus::Off,
            #[cfg(unix)]
            listener: None,
            config,
        };
        app.listen();
        app.refresh();
        app
    }

    #[cfg(unix)]
    fn listen(&mut self) {
        if !self.config.live {
            return;
        }
        match live::Listener::start(self.config.socket_dir.as_deref()) {
            Ok(listener) => {
                self.live = LiveStatus::Listening(listener.socket_path());
                self.listener = Some(listener);
            }
            Err(e) => self.live = LiveStatus::Failed(e),
        }
    }

    #[cfg(not(unix))]
    fn listen(&mut self) {}

    /// Looks for new transcripts, reads what was appended to the known ones
    /// and applies the live events that arrived.
    pub fn refresh(&mut self) {
        // New rows arrive at the top of the Tool calls and Subagents lists.
        // A cursor further down stays on its row; one on the first row keeps
        // following the latest.
        let tool = (self.tool_selected > 0)
            .then(|| self.selected_tool())
            .flatten()
            .map(|(session, call)| (session.id.clone(), call.id.clone()));
        let subagent = (self.subagent_selected > 0)
            .then(|| self.selected_subagent())
            .flatten()
            .map(|(session, agent)| (session.id.clone(), agent.agent_id.clone()));
        let call = (self.call_selected > 0)
            .then(|| self.selected_call())
            .flatten()
            .map(|call| call.id.clone());
        let listed = (self.trace_selected > 0)
            .then(|| self.selected_list_row())
            .flatten()
            .map(|row| {
                let interaction = row.interaction.map(|i| i.interaction.id.clone());
                (row.session.id.clone(), interaction)
            });
        let span = (self.span_selected > 0)
            .then(|| self.selected_span().map(|row| row.key()))
            .flatten();

        self.load();

        if let Some((session, id)) = tool {
            let found = self
                .tools()
                .iter()
                .position(|(s, c)| s.id == session && c.id == id);
            self.follow(Page::Tools, found);
        }
        if let Some((session, id)) = subagent {
            let found = self
                .subagents()
                .iter()
                .position(|(s, a)| s.id == session && a.agent_id == id);
            self.follow(Page::Subagents, found);
        }
        if let Some((session, id)) = listed {
            let found = self.trace_list().iter().position(|row| {
                row.session.id == session
                    && row.interaction.map(|i| &i.interaction.id) == id.as_ref()
            });
            self.follow(Page::Trace, found);
        }
        if let Some(key) = span {
            // Spans read since then may be above it in the tree.
            if let Some(to) = self.span_rows().iter().position(|row| row.key() == key) {
                self.offsets[TREE] = (self.offsets[TREE] + to).saturating_sub(self.span_selected);
                self.span_selected = to;
            }
        }
        if let Some(id) = call {
            if let Some(at) = self.subagent_calls().iter().position(|c| c.id == id) {
                self.call_selected = at;
            }
        }
    }

    /// Moves a list's cursor to where its row went, and the list's scroll
    /// position by as much, so the row stays where it was on screen.
    fn follow(&mut self, page: Page, found: Option<usize>) {
        let Some(to) = found else {
            return;
        };
        let index = page.list_index();
        let from = *self.cursor_mut(page);
        *self.cursor_mut(page) = to;
        self.offsets[index] = (self.offsets[index] + to).saturating_sub(from);
    }

    fn load(&mut self) {
        match discover(&self.config.projects_dir) {
            Ok(found) => {
                self.notice = None;
                for files in &found {
                    self.track(files);
                }
            }
            Err(e) => self.notice = Some(e.to_string()),
        }
        self.read_appended();
        #[cfg(unix)]
        if let Some(listener) = &self.listener {
            listener.drain(&mut self.store);
        }
        self.arrange(now());
    }

    fn track(&mut self, files: &SessionFiles) {
        self.store.discovered(files);
        let transcripts = std::iter::once(&files.transcript)
            .chain(files.subagents.iter().map(|subagent| &subagent.transcript));
        for transcript in transcripts {
            self.tails
                .entry(transcript.path.clone())
                .or_insert_with(|| (files.id.clone(), Tail::from_start(&transcript.path)));
        }
    }

    fn read_appended(&mut self) {
        let mut restarted = Vec::new();
        for (id, tail) in self.tails.values_mut() {
            let store = &mut self.store;
            let polled = tail.poll_each(|line, _| store.apply(id, &Record::parse_bytes(line)));
            // A transcript that vanished or cannot be read is skipped; it is
            // tried again on the next refresh.
            if polled.is_ok_and(|polled| polled.restarted) {
                restarted.push(id.clone());
            }
        }
        // A replaced file invalidates what was folded from it: forget the
        // session and read all of its files again.
        for id in restarted {
            self.store.reset(&id);
            for (owner, tail) in self.tails.values_mut() {
                if *owner == id {
                    *tail = Tail::from_start(tail.path());
                    let store = &mut self.store;
                    let _ = tail.poll_each(|line, _| store.apply(&id, &Record::parse_bytes(line)));
                }
            }
        }
    }

    /// Rebuilds the session list for the filter and the sort in force,
    /// keeping the cursor on the session it was on.
    pub fn arrange(&mut self, now: Timestamp) {
        let selected = self.order.get(self.selected).cloned();
        let mut sessions: Vec<&Session> = self
            .store
            .sessions()
            .filter(|s| self.filter.admits(status(s, now).kind))
            .collect();
        sessions.sort_by(|a, b| {
            let by = match self.sort {
                Sort::Updated => std::cmp::Ordering::Equal,
                // An unknown cost sorts below every known one.
                Sort::Cost => b
                    .cost
                    .usd
                    .partial_cmp(&a.cost.usd)
                    .unwrap_or(std::cmp::Ordering::Equal),
                Sort::Tokens => b
                    .usage
                    .total()
                    .total_tokens()
                    .cmp(&a.usage.total().total_tokens()),
            };
            by.then_with(|| latest(b).cmp(&latest(a)))
                .then_with(|| a.id.cmp(&b.id))
        });
        self.order = sessions.into_iter().map(|s| s.id.clone()).collect();
        let kept = selected.and_then(|id| self.order.iter().position(|other| *other == id));
        if kept.is_none() && self.page == Page::Sessions {
            // Another session is under the cursor now.
            self.reset_detail();
        }
        self.selected = kept.unwrap_or(0);
        if self
            .scope
            .as_ref()
            .is_some_and(|id| self.store.get(id).is_none())
        {
            self.scope = None;
            self.agent_scope = None;
        }
        self.clamp();
    }

    /// The session under the cursor of the Sessions page.
    pub fn selected_session(&self) -> Option<&Session> {
        self.order
            .get(self.selected)
            .and_then(|id| self.store.get(id))
    }

    /// The session the other pages are narrowed to.
    pub fn scoped_session(&self) -> Option<&Session> {
        self.scope.as_ref().and_then(|id| self.store.get(id))
    }

    /// The sessions the Tool calls and Subagents pages draw from.
    fn in_scope(&self) -> Vec<&Session> {
        match self.scoped_session() {
            Some(session) => vec![session],
            None => self.store.sessions().collect(),
        }
    }

    /// Recent tool calls, the latest first, each with its session.
    pub fn tools(&self) -> Vec<(&Session, &ToolCall)> {
        let mut calls: Vec<(&Session, &ToolCall)> = self
            .in_scope()
            .into_iter()
            .flat_map(|session| session.recent_tools.iter().map(move |call| (session, call)))
            .filter(|(_, call)| self.agent_scope.is_none() || call.agent_id == self.agent_scope)
            .collect();
        calls.sort_by(|a, b| latest_first(a.1, b.1));
        calls.truncate(TOOL_ROWS);
        calls
    }

    /// Subagents, the latest active first, each with its session.
    pub fn subagents(&self) -> Vec<(&Session, &Subagent)> {
        let mut subagents: Vec<(&Session, &Subagent)> = self
            .in_scope()
            .into_iter()
            .flat_map(|session| {
                session
                    .subagents
                    .values()
                    .map(move |agent| (session, agent))
            })
            .collect();
        subagents.sort_by(|a, b| {
            b.1.last_activity
                .cmp(&a.1.last_activity)
                .then_with(|| a.1.agent_id.cmp(&b.1.agent_id))
        });
        subagents
    }

    /// The Trace page's list. Flat: every interaction, the latest first. By
    /// session: the sessions that have interactions, the one with the
    /// latest first, and under each that is not folded its interactions,
    /// the latest first.
    pub fn trace_list(&self) -> Vec<ListRow<'_>> {
        if !self.by_session {
            let mut rows: Vec<ListRow> = self
                .in_scope()
                .into_iter()
                .flat_map(|session| {
                    session.trace.iter().map(move |interaction| ListRow {
                        session,
                        interaction: Some(interaction_row(session, interaction)),
                        open: true,
                    })
                })
                .collect();
            let started = |row: &ListRow| row.interaction.and_then(|i| i.interaction.started_at);
            rows.sort_by(|a, b| {
                started(b)
                    .cmp(&started(a))
                    .then_with(|| a.session.id.cmp(&b.session.id))
            });
            rows.truncate(INTERACTION_ROWS);
            return rows;
        }
        let latest = |session: &Session| session.trace.iter().filter_map(|i| i.started_at).max();
        let mut sessions: Vec<&Session> = self
            .in_scope()
            .into_iter()
            .filter(|session| !session.trace.is_empty())
            .collect();
        sessions.sort_by(|a, b| latest(b).cmp(&latest(a)).then_with(|| a.id.cmp(&b.id)));

        let mut rows = Vec::new();
        let mut left = INTERACTION_ROWS;
        for session in sessions {
            if left == 0 {
                break;
            }
            let open = !self.closed_sessions.contains(&session.id);
            rows.push(ListRow {
                session,
                interaction: None,
                open,
            });
            if !open {
                continue;
            }
            for interaction in session.trace.iter().rev().take(left) {
                rows.push(ListRow {
                    session,
                    interaction: Some(interaction_row(session, interaction)),
                    open,
                });
                left -= 1;
            }
        }
        rows
    }

    /// How many interactions the Trace page's sessions hold, folded or not.
    pub fn interaction_count(&self) -> usize {
        self.in_scope()
            .iter()
            .map(|session| session.trace.len())
            .sum()
    }

    pub fn selected_list_row(&self) -> Option<ListRow<'_>> {
        self.trace_list().get(self.trace_selected).copied()
    }

    /// The interaction the Trace page's other panes show: the one under the
    /// cursor, or, on a session's row, the session's latest.
    pub fn selected_interaction(&self) -> Option<InteractionRow<'_>> {
        let row = self.selected_list_row()?;
        row.interaction.or_else(|| {
            let latest = row.session.trace.back()?;
            Some(interaction_row(row.session, latest))
        })
    }

    /// `t`: the Trace page's list by session, or flat again. The cursor
    /// stays on its interaction (from a session's row, on its latest).
    pub fn toggle_by_session(&mut self) {
        if self.page != Page::Trace {
            return;
        }
        let on = self
            .selected_interaction()
            .map(|row| (row.session.id.clone(), row.interaction.id.clone()));
        self.by_session = !self.by_session;
        // The session of that interaction is unfolded, so it has a row.
        if let Some((session, _)) = &on {
            self.closed_sessions.remove(session);
        }
        let found = on.and_then(|(session, id)| {
            self.trace_list().iter().position(|row| {
                row.session.id == session && row.interaction.is_some_and(|i| i.interaction.id == id)
            })
        });
        self.trace_selected = found.unwrap_or(0);
        self.offsets[Page::Trace.list_index()] = 0;
    }

    /// Folds or unfolds the session under the cursor of the Trace page's
    /// list. `false` if the row is not a session's, or is already as asked.
    fn fold_session(&mut self, open: Option<bool>) -> bool {
        if self.page != Page::Trace || self.in_detail {
            return false;
        }
        let Some(row) = self
            .selected_list_row()
            .filter(|row| row.interaction.is_none())
        else {
            return false;
        };
        let open = open.unwrap_or(!row.open);
        if open == row.open {
            return false;
        }
        let id = row.session.id.clone();
        if open {
            self.closed_sessions.remove(&id);
        } else {
            self.closed_sessions.insert(id);
        }
        true
    }

    /// The interaction under the Trace page's cursor as a tree, in the order
    /// things happened: its LLM requests and tool calls, the subagents those
    /// spawned and, where one is unfolded, what it did.
    pub fn span_rows(&self) -> Vec<TraceRow<'_>> {
        let Some(InteractionRow {
            session,
            interaction,
            ..
        }) = self.selected_interaction()
        else {
            return Vec::new();
        };
        let nodes = session.walk(interaction);
        let interaction_end = interaction.ended_at.or_else(|| {
            nodes
                .iter()
                .filter_map(|n| n.ended_at().or(n.started_at()))
                .max()
        });
        let mut rows = Vec::new();
        // What is under a folded subagent is skipped.
        let mut folded: Option<usize> = None;
        for node in nodes {
            if folded.is_some_and(|depth| node.depth > depth) {
                continue;
            }
            folded = None;
            let (item, open) = match node.item {
                Item::Span(span) => (TraceItem::Span(span), None),
                Item::Subagent(subagent) => {
                    let key = (session.id.clone(), subagent.agent_id.clone());
                    let open = self.open_agents.contains(&key);
                    if !open {
                        folded = Some(node.depth);
                    }
                    (TraceItem::Subagent(subagent), Some(open))
                }
            };
            rows.push(TraceRow {
                session,
                interaction,
                interaction_end,
                depth: node.depth,
                item,
                open,
            });
        }
        rows
    }

    /// The row under the cursor of the interaction's tree.
    pub fn selected_span(&self) -> Option<TraceRow<'_>> {
        self.span_rows().get(self.span_selected).copied()
    }

    /// The Trace page's tree has the focus: its rows are chosen from.
    pub fn choosing_span(&self) -> bool {
        self.in_detail && self.page == Page::Trace && !self.call_focus
    }

    /// Folds or unfolds the subagent under the tree's cursor. `false` if
    /// the row is not one, or is already as asked.
    fn fold(&mut self, open: Option<bool>) -> bool {
        let Some(row) = self.selected_span() else {
            return false;
        };
        let (Some(is_open), TraceItem::Subagent(subagent)) = (row.open, row.item) else {
            return false;
        };
        let open = open.unwrap_or(!is_open);
        if open == is_open {
            return false;
        }
        let key = (row.session.id.clone(), subagent.agent_id.clone());
        if open {
            self.open_agents.insert(key);
        } else {
            self.open_agents.remove(&key);
        }
        true
    }

    /// `l`: on the Trace page, to the pane on the right, which shows the
    /// interaction (from the list) or the span chosen (from the tree).
    /// Elsewhere: one pane further in.
    pub fn right(&mut self) {
        if self.page != Page::Trace {
            self.focus_detail();
        } else if !self.in_detail {
            if self.selected_interaction().is_some() {
                self.in_detail = true;
                self.call_focus = true;
                self.from_list = true;
                self.call_scroll = 0;
            }
        } else if self.choosing_span() {
            self.call_focus = self.selected_span().is_some();
        }
    }

    /// The Trace page's tree shows its cursor: it has, or had before the
    /// pane of its span, the focus.
    pub fn in_tree(&self) -> bool {
        self.in_detail && self.page == Page::Trace && !self.from_list
    }

    pub fn selected_tool(&self) -> Option<(&Session, &ToolCall)> {
        self.tools().get(self.tool_selected).copied()
    }

    pub fn selected_subagent(&self) -> Option<(&Session, &Subagent)> {
        self.subagents().get(self.subagent_selected).copied()
    }

    /// The tool calls of the subagent under the cursor, the latest first:
    /// what its detail pane lists.
    pub fn subagent_calls(&self) -> Vec<&ToolCall> {
        self.selected_subagent()
            .map(|(session, agent)| calls_of(session, &agent.agent_id))
            .unwrap_or_default()
    }

    /// The tool call under the cursor of the subagent's detail pane.
    pub fn selected_call(&self) -> Option<&ToolCall> {
        self.subagent_calls().get(self.call_selected).copied()
    }

    /// The detail pane lists tool calls to choose from, not text to scroll,
    /// and it has the focus.
    pub fn choosing_call(&self) -> bool {
        self.in_detail && self.page == Page::Subagents && !self.call_focus
    }

    /// The pane showing what was chosen (a subagent's tool call, a span of
    /// the trace) has the focus.
    pub fn in_call(&self) -> bool {
        self.in_detail && self.call_focus
    }

    /// Another row is under the list's cursor: its detail starts at the top.
    fn reset_detail(&mut self) {
        self.detail_scroll = 0;
        self.call_selected = 0;
        self.call_scroll = 0;
        self.span_selected = 0;
        self.offsets[TREE] = 0;
    }

    fn list_len(&self, page: Page) -> usize {
        match page {
            Page::Sessions => self.order.len(),
            Page::Tools => self.tools().len(),
            Page::Subagents => self.subagents().len(),
            Page::Trace => self.trace_list().len(),
            Page::Usage => 0,
        }
    }

    fn cursor_mut(&mut self, page: Page) -> &mut usize {
        match page {
            Page::Sessions => &mut self.selected,
            Page::Tools => &mut self.tool_selected,
            Page::Subagents | Page::Usage => &mut self.subagent_selected,
            Page::Trace => &mut self.trace_selected,
        }
    }

    fn clamp(&mut self) {
        for page in [Page::Sessions, Page::Tools, Page::Subagents, Page::Trace] {
            let last = self.list_len(page).saturating_sub(1);
            let cursor = self.cursor_mut(page);
            *cursor = (*cursor).min(last);
        }
    }

    /// Moves the list's cursor, or scrolls the detail pane when it has the
    /// focus, to `to`, clamped to what there is.
    fn move_to(&mut self, to: usize) {
        if !self.page.has_list() {
            return;
        }
        if self.choosing_call() {
            // The pane scrolls to keep the call in view when it is drawn.
            let to = to.min(self.subagent_calls().len().saturating_sub(1));
            if self.call_selected != to {
                self.call_selected = to;
                self.call_scroll = 0;
            }
            return;
        }
        if self.choosing_span() {
            let to = to.min(self.span_rows().len().saturating_sub(1));
            if self.span_selected != to {
                self.span_selected = to;
                self.call_scroll = 0;
            }
            return;
        }
        if self.in_call() {
            let last = self
                .viewport
                .call_lines
                .saturating_sub(self.viewport.call_rows);
            self.call_scroll = to.min(last);
            return;
        }
        if self.in_detail {
            let last = self
                .viewport
                .detail_lines
                .saturating_sub(self.viewport.detail_rows);
            self.detail_scroll = to.min(last);
            return;
        }
        let page = self.page;
        let to = to.min(self.list_len(page).saturating_sub(1));
        if *self.cursor_mut(page) != to {
            *self.cursor_mut(page) = to;
            self.reset_detail();
        }
    }

    pub fn move_by(&mut self, by: isize) {
        let at = if self.choosing_call() {
            self.call_selected
        } else if self.choosing_span() {
            self.span_selected
        } else if self.in_call() {
            self.call_scroll
        } else if self.in_detail {
            self.detail_scroll
        } else {
            *self.cursor_mut(self.page)
        };
        self.move_to(at.saturating_add_signed(by));
    }

    pub fn move_first(&mut self) {
        self.move_to(0);
    }

    pub fn move_last(&mut self) {
        self.move_to(usize::MAX);
    }

    /// Half of what the focused pane shows, as `Ctrl+d` / `Ctrl+u` move.
    pub fn half_page(&self) -> isize {
        let rows = if self.in_call() {
            self.viewport.call_rows
        } else if self.in_detail {
            self.viewport.detail_rows
        } else {
            self.viewport.list_rows
        };
        (rows / 2).max(1) as isize
    }

    pub fn go_to(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.in_detail = false;
            self.call_focus = false;
            self.from_list = false;
            self.reset_detail();
        }
    }

    /// Whether there is something chosen for the page's third pane to show:
    /// a subagent's tool call, a span of the trace.
    fn has_chosen(&self) -> bool {
        (self.choosing_call() && self.selected_call().is_some())
            || (self.choosing_span() && self.selected_span().is_some())
    }

    /// `Tab` / `Shift+Tab`: the page's panes in turn. The Subagents and
    /// Trace pages have a third, for what is chosen in the second.
    pub fn toggle_focus(&mut self) {
        if !self.page.has_list() {
            return;
        }
        if self.has_chosen() {
            self.call_focus = true;
        } else {
            self.in_detail = !self.in_detail;
            self.call_focus = false;
            self.from_list = false;
        }
    }

    /// `i`: into the detail pane; from a subagent's, or from the trace's
    /// tree, into the pane of what is chosen there.
    pub fn focus_detail(&mut self) {
        if !self.page.has_list() {
            return;
        }
        if self.in_detail && !self.call_focus {
            self.call_focus = self.has_chosen();
        }
        self.in_detail = true;
    }

    /// One pane back towards the list. `false` on the list itself.
    fn step_out(&mut self) -> bool {
        if self.in_call() {
            self.call_focus = false;
            // Back to where the pane was entered from.
            if std::mem::take(&mut self.from_list) {
                self.in_detail = false;
            }
        } else if self.in_detail {
            self.in_detail = false;
        } else {
            return false;
        }
        true
    }

    /// The subagent the Tool calls page is narrowed to.
    pub fn scoped_subagent(&self) -> Option<&Subagent> {
        let agent = self.agent_scope.as_ref()?;
        self.scoped_session()?.subagents.get(agent)
    }

    /// `Enter`. On a session: narrow the Tool calls and Subagents pages to it
    /// and show its tool calls. On a subagent: narrow the Tool calls page to
    /// it and show them. On a session in the Trace page's list, or a
    /// subagent in its tree: fold or unfold it. Elsewhere: one pane further
    /// in.
    pub fn enter(&mut self) {
        if self.in_detail {
            if !(self.choosing_span() && self.fold(None)) {
                self.focus_detail();
            }
            return;
        }
        match self.page {
            Page::Trace => {
                if !self.fold_session(None) {
                    self.focus_detail();
                }
            }
            Page::Sessions => {
                let Some(id) = self.order.get(self.selected).cloned() else {
                    return;
                };
                self.narrow(Some(id), None);
                self.go_to(Page::Tools);
            }
            Page::Subagents => {
                let Some((session, agent)) = self
                    .selected_subagent()
                    .map(|(s, a)| (s.id.clone(), a.agent_id.clone()))
                else {
                    return;
                };
                // Narrowing to the session may reorder this list: stay on
                // the subagent.
                self.narrow(Some(session), Some(agent.clone()));
                if let Some(at) = self
                    .subagents()
                    .iter()
                    .position(|(_, a)| a.agent_id == agent)
                {
                    self.subagent_selected = at;
                }
                self.go_to(Page::Tools);
            }
            _ => self.focus_detail(),
        }
    }

    fn narrow(&mut self, scope: Option<String>, agent: Option<String>) {
        if self.scope == scope && self.agent_scope == agent {
            return;
        }
        if self.scope != scope {
            self.subagent_selected = 0;
            self.offsets[2] = 0;
            self.trace_selected = 0;
            self.offsets[3] = 0;
        }
        self.scope = scope;
        self.agent_scope = agent;
        self.tool_selected = 0;
        self.offsets[1] = 0;
        if self.page != Page::Sessions {
            self.reset_detail();
        }
        // A search's matches were rows of the old lists.
        if self
            .search
            .as_ref()
            .is_some_and(|s| s.page != Page::Sessions)
        {
            self.search = None;
        }
    }

    /// `Esc`: out of the pane in focus. On a list it drops the search; with
    /// no search, the narrowing to a subagent, then the narrowing to a session.
    pub fn back(&mut self) {
        if self.step_out() {
            return;
        }
        if self.search.as_ref().is_some_and(|s| s.page == self.page) {
            self.search = None;
        } else if self.agent_scope.is_some() {
            self.narrow(self.scope.clone(), None);
        } else {
            self.narrow(None, None);
        }
    }

    /// `h`: out of the pane in focus only. In the Trace page's tree, and in
    /// its list by session, it first goes to the row this one is under.
    pub fn leave_detail(&mut self) {
        if self.choosing_span() {
            let rows = self.span_rows();
            let parent = rows.get(self.span_selected).and_then(|row| {
                rows[..self.span_selected]
                    .iter()
                    .rposition(|above| above.depth < row.depth)
            });
            if let Some(parent) = parent {
                self.move_to(parent);
                return;
            }
        }
        if self.page == Page::Trace && !self.in_detail {
            let rows = self.trace_list();
            let on_interaction = rows
                .get(self.trace_selected)
                .is_some_and(|row| row.interaction.is_some());
            let session = rows[..self.trace_selected.min(rows.len())]
                .iter()
                .rposition(|row| row.interaction.is_none());
            if let (true, Some(session)) = (on_interaction, session) {
                self.move_to(session);
            }
            return;
        }
        self.step_out();
    }

    /// How times are shown now.
    pub fn times(&self) -> Times {
        Times {
            style: self.time,
            ago_within: self.config.ago_within,
        }
    }

    /// `T`: the lists' times as a date and time, or as how long ago.
    pub fn toggle_time(&mut self) {
        self.time = match self.time {
            TimeStyle::Clock => TimeStyle::Relative,
            TimeStyle::Relative => TimeStyle::Clock,
        };
    }

    pub fn cycle_sort(&mut self) {
        if self.page != Page::Sessions {
            return;
        }
        self.sort = match self.sort {
            Sort::Updated => Sort::Cost,
            Sort::Cost => Sort::Tokens,
            Sort::Tokens => Sort::Updated,
        };
        self.arrange(now());
    }

    pub fn cycle_filter(&mut self) {
        if self.page != Page::Sessions {
            return;
        }
        self.filter = match self.filter {
            Filter::All => Filter::Live,
            Filter::Live => Filter::Open,
            Filter::Open => Filter::All,
        };
        self.arrange(now());
    }

    /// The text each row of `page` is searched by, lower-cased.
    fn haystacks(&self, page: Page, now: Timestamp) -> Vec<String> {
        let rows: Vec<String> = match page {
            Page::Sessions => self
                .order
                .iter()
                .filter_map(|id| self.store.get(id))
                .map(|s| {
                    format!(
                        "{} {} {} {} {}",
                        label(s),
                        project(s).unwrap_or_default(),
                        s.git_branch.as_deref().unwrap_or_default(),
                        status(s, now).text,
                        s.id
                    )
                })
                .collect(),
            Page::Tools => self
                .tools()
                .iter()
                .map(|(s, c)| {
                    format!(
                        "{} {} {}",
                        c.name,
                        c.summary.as_deref().unwrap_or_default(),
                        label(s)
                    )
                })
                .collect(),
            Page::Subagents => self
                .subagents()
                .iter()
                .map(|(s, a)| {
                    format!(
                        "{} {} {} {}",
                        a.agent_type.as_deref().unwrap_or_default(),
                        a.description.as_deref().unwrap_or_default(),
                        a.agent_id,
                        label(s)
                    )
                })
                .collect(),
            Page::Trace => self.trace_list().iter().map(ListRow::text).collect(),
            Page::Usage => Vec::new(),
        };
        rows.into_iter().map(|row| row.to_lowercase()).collect()
    }

    /// The rows of `page` the search in force matches.
    pub fn matches(&self, page: Page, now: Timestamp) -> Vec<usize> {
        let Some(search) = self.search.as_ref().filter(|s| s.page == page) else {
            return Vec::new();
        };
        self.haystacks(page, now)
            .iter()
            .enumerate()
            .filter(|(_, row)| row.contains(&search.query))
            .map(|(i, _)| i)
            .collect()
    }

    /// Starts typing a search in the page's list.
    pub fn start_search(&mut self) {
        if self.page.has_list() && !self.in_detail {
            self.typing = Some(String::new());
        }
    }

    /// `Enter` while typing: search, and go to the first match from the
    /// cursor on.
    pub fn submit_search(&mut self) {
        let Some(typed) = self.typing.take() else {
            return;
        };
        let query = typed.trim().to_lowercase();
        if query.is_empty() {
            self.search = None;
            return;
        }
        let page = self.page;
        self.search = Some(Search { page, query });
        let at = *self.cursor_mut(page);
        let matches = self.matches(page, now());
        if let Some(to) = matches.iter().find(|m| **m >= at).or(matches.first()) {
            self.move_to(*to);
        }
    }

    /// `n` / `N`: the next or the previous match, wrapping around.
    pub fn next_match(&mut self, by: isize) {
        if self.in_detail {
            return;
        }
        let page = self.page;
        let matches = self.matches(page, now());
        let at = *self.cursor_mut(page);
        let to = if by > 0 {
            matches.iter().find(|m| **m > at).or(matches.first())
        } else {
            matches.iter().rev().find(|m| **m < at).or(matches.last())
        };
        if let Some(to) = to {
            self.move_to(*to);
        }
    }
}

/// The order of the tool call lists: the latest first.
fn latest_first(a: &ToolCall, b: &ToolCall) -> std::cmp::Ordering {
    b.started_at
        .cmp(&a.started_at)
        .then_with(|| a.id.cmp(&b.id))
}

/// The recent tool calls a subagent of `session` made, the latest first.
pub fn calls_of<'a>(session: &'a Session, agent_id: &str) -> Vec<&'a ToolCall> {
    let mut calls: Vec<&ToolCall> = session
        .recent_tools
        .iter()
        .filter(|call| call.agent_id.as_deref() == Some(agent_id))
        .collect();
    calls.sort_by(|a, b| latest_first(a, b));
    calls
}

/// An interaction as the Trace page lists it.
fn interaction_row<'a>(session: &'a Session, interaction: &'a Interaction) -> InteractionRow<'a> {
    InteractionRow {
        session,
        interaction,
        end: interaction.ended_at.or_else(|| {
            let spans = interaction.thread.spans.iter();
            spans.filter_map(|s| s.ended_at.or(s.started_at)).max()
        }),
    }
}

/// The time of the last thing known about a session, from either input.
pub fn latest(session: &Session) -> Option<Timestamp> {
    let live = session
        .live
        .as_ref()
        .and_then(|l| l.state_at.max(l.measured_at));
    session.last_activity.max(live)
}

/// The current time.
pub fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    Timestamp::from_millis(millis)
}

/// What a session is doing: from live events when they say, from the
/// transcript otherwise.
pub fn status(session: &Session, now: Timestamp) -> Status {
    let status = |kind, text: &str| Status {
        kind,
        text: text.to_owned(),
    };
    // A transcript that ends with the exit bookkeeping outranks older events:
    // the mod's `session.end` may never have been delivered.
    let live = session
        .live
        .as_ref()
        .filter(|live| !(session.exited && live.state_at <= session.last_activity));
    if let Some(live) = live {
        match live.state() {
            LiveState::Ended => return status(StatusKind::Exited, "exited"),
            LiveState::AwaitingApproval(pending) => {
                return Status {
                    kind: StatusKind::NeedsApproval,
                    text: format!("approve {}?", pending.name),
                }
            }
            LiveState::RunningTool(tool) => {
                return Status {
                    kind: StatusKind::Busy,
                    text: format!("running {}", tool.name),
                }
            }
            LiveState::Working => return status(StatusKind::Busy, "working"),
            LiveState::WaitingForInput => return status(StatusKind::Waiting, "waiting"),
            LiveState::Unknown => {}
        }
    }
    match session.activity(now, ACTIVE_WITHIN) {
        Activity::Active { .. } => status(StatusKind::Busy, "active"),
        Activity::Idle => status(StatusKind::Idle, "idle"),
        Activity::Exited => status(StatusKind::Exited, "exited"),
    }
}

/// The name to show for a session: its title, else its last prompt, else the
/// start of its id.
pub fn label(session: &Session) -> &str {
    session
        .title
        .as_deref()
        .or(session.last_prompt.as_deref())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| session.id.get(..8).unwrap_or(&session.id))
}

/// The last component of where the session runs.
pub fn project(session: &Session) -> Option<String> {
    let live_cwd = session.live.as_ref().and_then(|l| l.cwd.as_ref());
    session
        .project_path
        .as_ref()
        .or(session.cwd.as_ref())
        .or(live_cwd)
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(unix)]
mod live {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Receiver;

    use iktinos::session::SessionStore;
    use iktinos_live::event::Event;
    use iktinos_live::merge::apply_event;
    use iktinos_live::server::Server;

    /// The socket server and the events it has received.
    pub struct Listener {
        server: Server,
        events: Receiver<Event>,
    }

    impl Listener {
        pub fn start(socket_dir: Option<&Path>) -> Result<Self, String> {
            let started = match socket_dir {
                Some(dir) => Server::start_in(dir),
                None => Server::start(),
            };
            let (server, events) = started.map_err(|e| e.to_string())?;
            Ok(Self { server, events })
        }

        pub fn socket_path(&self) -> PathBuf {
            self.server.socket_path().to_owned()
        }

        pub fn drain(&self, store: &mut SessionStore) {
            for event in self.events.try_iter() {
                apply_event(store, &event);
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use iktinos::record::KNOWN_CLAUDE_CODE_VERSION;
    use iktinos::trace::SpanKind;
    use std::path::Path;

    pub const INDEXED: &str = "11111111-1111-4111-8111-111111111111";
    pub const WITH_SUBAGENTS: &str = "22222222-2222-4222-8222-222222222222";
    pub const UNINDEXED: &str = "33333333-3333-4333-8333-333333333333";

    pub fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    pub fn fixture_app() -> App {
        App::new(Config {
            projects_dir: fixtures().join(KNOWN_CLAUDE_CODE_VERSION).join("projects"),
            live: false,
            socket_dir: None,
            time: TimeStyle::default(),
            ago_within: DEFAULT_AGO_WITHIN,
        })
    }

    #[test]
    fn sessions_are_loaded_and_ordered_by_latest_activity() {
        let app = fixture_app();
        assert_eq!(app.notice, None);
        assert_eq!(app.live, LiveStatus::Off);
        // 2026-11-03, 2026-11-02, 2026-11-01.
        assert_eq!(app.order, [UNINDEXED, WITH_SUBAGENTS, INDEXED]);
        assert_eq!(app.selected_session().unwrap().id, UNINDEXED);

        let session = app.store.get(WITH_SUBAGENTS).unwrap();
        assert_eq!(session.usage.total().responses, 7);
        assert_eq!(session.subagents.len(), 2);
        assert_eq!(project(session).as_deref(), Some("other.repo"));
        assert_eq!(label(session), "22222222");
        assert_eq!(
            label(app.store.get(INDEXED).unwrap()),
            "Synthetic session title"
        );
    }

    #[test]
    fn a_refresh_reads_nothing_twice() {
        let mut app = fixture_app();
        let before = app.store.get(WITH_SUBAGENTS).unwrap().clone();
        app.refresh();
        app.refresh();
        assert_eq!(app.store.get(WITH_SUBAGENTS).unwrap(), &before);
        assert_eq!(app.order.len(), 3);
    }

    #[test]
    fn the_cursor_moves_within_the_list_and_follows_its_session() {
        let mut app = fixture_app();
        app.move_by(-1);
        assert_eq!(app.selected, 0);
        app.move_last();
        assert_eq!(app.selected, 2);
        app.move_by(1);
        assert_eq!(app.selected, 2);
        app.move_first();
        app.move_by(1);
        assert_eq!(app.selected_session().unwrap().id, WITH_SUBAGENTS);
        // The list is re-sorted on refresh; the cursor stays on its session.
        app.refresh();
        assert_eq!(app.selected_session().unwrap().id, WITH_SUBAGENTS);
    }

    #[test]
    fn the_other_pages_list_every_session_until_one_is_entered() {
        let mut app = fixture_app();
        // Unscoped: everything, the latest first.
        assert_eq!(app.scope, None);
        assert_eq!(app.tools().len(), 5);
        assert_eq!(app.subagents().len(), 2);
        let (session, call) = app.tools()[4];
        assert_eq!(
            (session.id.as_str(), call.name.as_str()),
            (WITH_SUBAGENTS, "Bash")
        );

        // Enter on a session narrows the other pages to it and shows its
        // tool calls.
        app.enter(); // the damaged transcript: no tool calls at all
        assert_eq!(app.page, Page::Tools);
        assert_eq!(app.scope.as_deref(), Some(UNINDEXED));
        assert!(app.tools().is_empty());
        assert!(app.subagents().is_empty());
        assert!(app.selected_tool().is_none());

        app.go_to(Page::Sessions);
        app.move_by(1);
        app.enter();
        assert_eq!(app.scope.as_deref(), Some(WITH_SUBAGENTS));
        assert_eq!(app.tools().len(), 5);
        app.move_by(3);
        assert_eq!(app.selected_tool().unwrap().1.name, "Read");
        app.go_to(Page::Subagents);
        app.move_by(1);
        assert_eq!(
            app.selected_subagent().unwrap().1.agent_id,
            "b2c3d4e5f6071829"
        );
        app.go_to(Page::Tools);
        assert_eq!(app.tool_selected, 3, "each page keeps its cursor");

        // Esc on a list drops the narrowing, and the cursors start again.
        app.back();
        assert_eq!(app.scope, None);
        assert_eq!((app.tool_selected, app.subagent_selected), (0, 0));
        assert_eq!(app.page, Page::Tools);
    }

    /// A copy of the fixture projects that a test may append to.
    pub fn scratch_projects(name: &str) -> PathBuf {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.path().is_dir() {
                    copy(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("iktinos-tui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy(
            &fixtures().join(KNOWN_CLAUDE_CODE_VERSION).join("projects"),
            &dir,
        );
        dir
    }

    /// Appends one more tool call, later than every other, to the session
    /// with subagents.
    fn append_tool_call(projects: &Path, id: &str, minute: u8) {
        append_call(
            projects,
            id,
            minute,
            "Edit",
            r#"{"file_path":"/work/other.repo/src/lib.rs"}"#,
        );
    }

    /// Appends a call of `name` with `input` (JSON) to the same session.
    pub fn append_call(projects: &Path, id: &str, minute: u8, name: &str, input: &str) {
        use std::io::Write;
        let line = format!(
            r#"{{"parentUuid":"a4","isSidechain":false,"type":"assistant","message":{{"model":"claude-fable-5-2","id":"msg_{id}","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}],"stop_reason":"tool_use","usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}},"uuid":"uuid-{id}","timestamp":"2026-11-02T11:{minute:02}:00.000Z","cwd":"/work/other.repo","sessionId":"{WITH_SUBAGENTS}","version":"2.1.325","gitBranch":"main"}}"#
        );
        let path = projects
            .join("-work-other-repo")
            .join(format!("{WITH_SUBAGENTS}.jsonl"));
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    /// Appends a call of the Explore subagent, later than its others.
    pub fn append_agent_call(projects: &Path, id: &str, minute: u8, input: &str) {
        use std::io::Write;
        const AGENT: &str = "a1b2c3d4e5f60718";
        let line = format!(
            r#"{{"parentUuid":"sa2","isSidechain":true,"type":"assistant","message":{{"model":"claude-haiku-5-5","id":"msg_{id}","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"Bash","input":{input}}}],"stop_reason":"tool_use","usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}},"uuid":"uuid-{id}","timestamp":"2026-11-02T11:{minute:02}:00.000Z","cwd":"/work/other.repo","sessionId":"{WITH_SUBAGENTS}","agentId":"{AGENT}","version":"2.1.325","gitBranch":"main"}}"#
        );
        let path = projects
            .join("-work-other-repo")
            .join(WITH_SUBAGENTS)
            .join("subagents")
            .join(format!("agent-{AGENT}.jsonl"));
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    #[test]
    fn a_call_in_a_subagents_detail_is_chosen_and_shown_beside_it() {
        let projects = scratch_projects("calls");
        append_agent_call(&projects, "toolu_s2", 0, r#"{"command":"ls"}"#);
        append_agent_call(&projects, "toolu_s3", 1, r#"{"command":"pwd"}"#);
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
            time: TimeStyle::default(),
            ago_within: DEFAULT_AGO_WITHIN,
        });
        app.go_to(Page::Subagents);
        assert_eq!(
            app.selected_subagent().unwrap().1.agent_id,
            "a1b2c3d4e5f60718"
        );
        let ids = |app: &App| -> Vec<String> {
            app.subagent_calls().iter().map(|c| c.id.clone()).collect()
        };
        assert_eq!(ids(&app), ["toolu_s3", "toolu_s2", "toolu_s1"]);

        // On the list, moving changes the subagent, not the call.
        assert!(!app.choosing_call());
        app.focus_detail();
        assert!(app.choosing_call());
        app.move_by(1);
        assert_eq!((app.subagent_selected, app.call_selected), (0, 1));
        app.move_last();
        assert_eq!(app.call_selected, 2);
        app.move_by(1);
        assert_eq!(app.call_selected, 2);
        app.move_by(-1);
        assert_eq!(app.selected_call().unwrap().id, "toolu_s2");

        // A newer call arrives above: the cursor stays on its call.
        append_agent_call(&projects, "toolu_s4", 2, r#"{"command":"date"}"#);
        app.refresh();
        assert_eq!(app.selected_call().unwrap().id, "toolu_s2");
        assert_eq!(app.call_selected, 2);

        // Enter, `i` or Tab: the pane showing that call, which scrolls.
        app.enter();
        assert!(app.in_call() && !app.choosing_call());
        app.viewport.call_rows = 4;
        app.viewport.call_lines = 10;
        app.move_by(2);
        app.move_by(app.half_page());
        assert_eq!((app.call_scroll, app.call_selected), (4, 2));
        app.move_last();
        assert_eq!(app.call_scroll, 6);
        app.enter();
        assert!(app.in_call(), "there is nothing further in");

        // Back out a pane at a time; another call starts at its top.
        app.leave_detail();
        assert!(app.choosing_call());
        assert_eq!(app.call_scroll, 6);
        app.move_by(-1);
        assert_eq!((app.call_selected, app.call_scroll), (1, 0));
        app.focus_detail();
        assert!(app.in_call());
        app.back();
        app.back();
        assert!(!app.in_detail);
        assert_eq!(app.page, Page::Subagents);
        // Tab goes round the three panes.
        app.toggle_focus();
        assert!(app.choosing_call());
        app.toggle_focus();
        assert!(app.in_call());
        app.toggle_focus();
        assert!(!app.in_detail && !app.call_focus);

        // Another subagent's detail starts on its first call.
        app.move_by(1);
        app.focus_detail();
        app.move_by(3);
        assert_eq!(app.call_selected, 0, "it made one call");
        // Leaving the page from the call pane leaves the pane too.
        app.focus_detail();
        assert!(app.in_call());
        app.go_to(Page::Tools);
        app.go_to(Page::Subagents);
        assert!(!app.in_detail && !app.in_call());

        let _ = std::fs::remove_dir_all(projects);
    }

    /// Appends a prompt, later than everything else, to the session with
    /// subagents: a new interaction.
    pub fn append_prompt(projects: &Path, id: &str, minute: u8) {
        use std::io::Write;
        let line = format!(
            r#"{{"parentUuid":null,"isSidechain":false,"promptId":"{id}","type":"user","message":{{"role":"user","content":"Synthetic prompt {id}\nSYNTHETIC-TEXT invented for the fixture"}},"uuid":"uuid-{id}","timestamp":"2026-11-02T11:{minute:02}:00.000Z","cwd":"/work/other.repo","sessionId":"{WITH_SUBAGENTS}","version":"2.1.325","gitBranch":"main"}}"#
        );
        let path = projects
            .join("-work-other-repo")
            .join(format!("{WITH_SUBAGENTS}.jsonl"));
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    /// The rows of the Trace page's list: an interaction by its id, a
    /// session by the start of its own, with how it folds.
    fn listed(app: &App) -> Vec<String> {
        app.trace_list()
            .iter()
            .map(|row| match row.interaction {
                Some(interaction) => interaction.interaction.id.clone(),
                None => format!(
                    "{} {}",
                    if row.open { "-" } else { "+" },
                    &row.session.id[..8]
                ),
            })
            .collect()
    }

    /// The tree of the interaction under the cursor as text: indented, with how
    /// its rows fold.
    fn tree(app: &App) -> Vec<String> {
        app.span_rows()
            .iter()
            .map(|row| {
                let what = match row.item {
                    TraceItem::Span(span) => match &span.kind {
                        SpanKind::Tool(call) => call.name.clone(),
                        _ => "llm_request".to_owned(),
                    },
                    TraceItem::Subagent(subagent) => format!("agent {}", subagent.agent_id),
                };
                let fold = match row.open {
                    Some(true) => "- ",
                    Some(false) => "+ ",
                    None => "",
                };
                format!("{}{fold}{what}", "  ".repeat(row.depth))
            })
            .collect()
    }

    #[test]
    fn the_trace_page_lists_interactions_over_the_tree_of_one() {
        let mut app = fixture_app();
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
                                // Every session's interactions, the latest first.
        assert_eq!(listed(&app).len(), 3);
        assert_eq!(app.selected_interaction().unwrap().session.id, UNINDEXED);
        assert!(tree(&app).is_empty(), "only its prompt was read");

        // The tree under the list is the interaction under the cursor: its
        // LLM requests and tool calls, the subagent under the call that
        // spawned it and the one placed by time.
        app.move_by(1);
        assert_eq!(
            app.selected_interaction().unwrap().session.id,
            WITH_SUBAGENTS
        );
        assert_eq!(
            tree(&app),
            [
                "llm_request",
                "Bash",
                "Read",
                "  + agent a1b2c3d4e5f60718",
                "llm_request",
                "mcp__example__lookup",
                "llm_request",
                "+ agent b2c3d4e5f6071829",
            ]
        );
        app.move_by(1);
        assert_eq!(tree(&app), ["llm_request"]);

        // Narrowed to a session like the pages before it.
        app.go_to(Page::Sessions);
        app.move_by(1);
        app.enter();
        app.go_to(Page::Trace);
        assert_eq!(listed(&app), ["prompt-u1"]);

        // Enter goes into the tree, where the cursor is on a span.
        app.enter();
        assert!(app.choosing_span() && app.in_tree());
        app.move_by(3);
        assert_eq!((app.trace_selected, app.span_selected), (0, 3));
        // Enter unfolds a subagent, and folds it again.
        app.enter();
        assert!(app.choosing_span());
        assert_eq!(
            tree(&app)[3..7],
            [
                "  - agent a1b2c3d4e5f60718",
                "    llm_request",
                "    Grep",
                "    llm_request",
            ]
        );
        app.enter();
        assert_eq!(tree(&app).len(), 8);
        app.enter();
        app.move_by(2);
        assert_eq!(app.selected_span().unwrap().key(), "t:toolu_s1");

        // `l` is the pane on the right, which shows the span and scrolls;
        // it does not unfold anything. Enter on a row that does not fold
        // goes there too.
        app.right();
        assert!(app.in_call() && !app.choosing_span() && app.in_tree());
        app.viewport.call_rows = 4;
        app.viewport.call_lines = 10;
        app.move_by(3);
        assert_eq!((app.call_scroll, app.span_selected), (3, 5));
        app.right();
        assert!(app.in_call(), "there is nothing further right");
        app.leave_detail();
        assert!(app.choosing_span());
        app.enter();
        assert!(app.in_call());
        app.back();
        app.move_by(-2);
        app.right();
        assert!(app.in_call(), "on a subagent too");
        assert_eq!(tree(&app).len(), 11, "and it stays unfolded");
        app.leave_detail();
        app.move_by(2);

        // `h` goes to the row this one is under, then out of the tree; it
        // does not fold anything.
        app.leave_detail();
        assert_eq!(app.span_selected, 3, "the subagent that made the call");
        assert_eq!(app.call_scroll, 0, "another span starts at its top");
        app.leave_detail();
        assert_eq!(app.span_selected, 2, "the call that spawned it");
        assert_eq!(tree(&app).len(), 11);
        app.leave_detail();
        assert!(
            !app.in_detail,
            "a call is under nothing but the interaction"
        );

        // From the list, `l` is the same pane, showing the interaction, and
        // `h` or Esc comes back to the list.
        app.right();
        assert!(app.in_call() && app.from_list && !app.in_tree());
        app.move_by(2);
        assert_eq!((app.call_scroll, app.trace_selected), (2, 0));
        app.leave_detail();
        assert!(!app.in_detail && !app.from_list);
        app.right();
        app.back();
        assert!(!app.in_detail && !app.from_list);
        assert!(app.scope.is_some(), "Esc left the pane, not the narrowing");

        // Tab goes round the three panes.
        app.toggle_focus();
        assert!(app.choosing_span());
        app.toggle_focus();
        assert!(app.in_call() && !app.from_list);
        app.toggle_focus();
        assert!(!app.in_detail && !app.call_focus);

        // A search finds an interaction by what it called.
        app.back();
        assert_eq!(app.scope, None);
        app.start_search();
        app.typing = Some("lookup".to_owned());
        app.submit_search();
        assert_eq!(app.matches(Page::Trace, now()), [1]);
        assert_eq!(app.trace_selected, 1);
        // Another interaction's tree starts at its top.
        app.focus_detail();
        app.move_by(2);
        app.leave_detail();
        app.move_by(1);
        app.focus_detail();
        assert_eq!(app.span_selected, 0);
    }

    #[test]
    fn the_trace_list_can_be_a_tree_of_sessions() {
        let mut app = fixture_app();
        assert!(app.by_session, "by session to begin with");
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        app.move_by(1);
        let flat = listed(&app);
        assert_eq!(flat.len(), 3);
        assert!(flat.iter().all(|row| !row.starts_with(['-', '+'])));

        // `t`: each session with its interactions under it, and the cursor
        // still on the interaction it was on.
        app.toggle_by_session();
        assert!(app.by_session);
        assert_eq!(
            listed(&app),
            [
                "- 33333333",
                flat[0].as_str(),
                "- 22222222",
                "prompt-u1",
                "- 11111111",
                flat[2].as_str(),
            ]
        );
        assert_eq!(app.trace_selected, 3);
        assert_eq!(tree(&app).len(), 8);

        // On a session's row the other panes show its latest interaction.
        app.leave_detail(); // `h`: to the session the interaction is under
        assert_eq!(app.trace_selected, 2);
        assert_eq!(
            app.selected_interaction().unwrap().interaction.id,
            "prompt-u1"
        );
        assert_eq!(tree(&app).len(), 8);
        // `h` and `l` fold nothing.
        app.leave_detail();
        assert_eq!(listed(&app).len(), 6);
        app.right();
        assert!(app.in_call() && app.from_list);
        assert_eq!(listed(&app).len(), 6);
        app.leave_detail();

        // Enter folds a session and unfolds it; on an interaction it goes
        // into the tree.
        app.enter();
        assert_eq!(
            listed(&app),
            [
                "- 33333333",
                flat[0].as_str(),
                "+ 22222222",
                "- 11111111",
                flat[2].as_str(),
            ]
        );
        assert!(!app.in_detail);
        assert_eq!(tree(&app).len(), 8, "its latest interaction still");
        app.enter();
        assert_eq!(listed(&app).len(), 6);
        app.move_by(1);
        app.enter();
        assert!(app.choosing_span());
        app.back();

        // A search matches a session by its name and an interaction by
        // what it called.
        app.start_search();
        app.typing = Some("other.repo".to_owned());
        app.submit_search();
        assert_eq!(app.matches(Page::Trace, now()), [2]);
        app.back();
        app.start_search();
        app.typing = Some("lookup".to_owned());
        app.submit_search();
        assert_eq!(app.matches(Page::Trace, now()), [3]);
        app.back();

        // `t` again: flat, on the same interaction. From a session's row,
        // on its latest; a folded session is unfolded for it.
        app.move_first();
        app.move_by(2);
        app.enter();
        assert_eq!(listed(&app)[2], "+ 22222222");
        app.toggle_by_session();
        assert!(!app.by_session);
        assert_eq!(listed(&app), flat);
        assert_eq!(app.trace_selected, 1);
        app.toggle_by_session();
        assert_eq!(listed(&app)[2..4], ["- 22222222", "prompt-u1"]);
        assert_eq!(app.trace_selected, 3);

        // Narrowed to a session, the tree has that session only.
        app.go_to(Page::Sessions);
        app.move_first();
        app.move_by(1);
        app.enter();
        app.go_to(Page::Trace);
        assert_eq!(listed(&app), ["- 22222222", "prompt-u1"]);
        // `t` belongs to the Trace page.
        app.go_to(Page::Tools);
        app.toggle_by_session();
        assert!(app.by_session);
    }

    #[test]
    fn new_interactions_and_spans_do_not_move_the_trace_cursors() {
        let projects = scratch_projects("trace-follow");
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
            time: TimeStyle::default(),
            ago_within: DEFAULT_AGO_WITHIN,
        });
        app.go_to(Page::Trace);
        app.by_session = false; // these are about the flat list
        app.move_by(1);
        app.enter();
        app.move_by(3);
        app.enter(); // the subagent, unfolded
        app.move_by(4);
        assert_eq!(
            app.selected_interaction().unwrap().interaction.id,
            "prompt-u1"
        );
        assert_eq!(app.selected_span().unwrap().key(), "r:msg_2:1793613603800");
        assert_eq!(app.span_selected, 7);

        // A newer interaction arrives above it in the list (under the one of a
        // session that is a day later), and the subagent above the cursor
        // makes one more call.
        append_prompt(&projects, "prompt-new", 0);
        append_tool_call(&projects, "toolu_new1", 1);
        append_agent_call(&projects, "toolu_s2", 2, r#"{"command":"ls"}"#);
        app.refresh();
        assert_eq!(listed(&app)[1..3], ["prompt-new", "prompt-u1"]);
        assert_eq!(
            app.selected_interaction().unwrap().interaction.id,
            "prompt-u1"
        );
        assert_eq!(app.trace_selected, 2);
        assert_eq!(app.selected_span().unwrap().key(), "r:msg_2:1793613603800");
        assert_eq!(app.span_selected, 9);

        // The new call is in the new interaction, not in the old one.
        assert!(!tree(&app).contains(&"Edit".to_owned()));
        app.back();
        app.move_by(-1);
        assert_eq!(tree(&app), ["llm_request", "Edit"]);

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn a_new_row_does_not_move_the_cursor_off_its_row() {
        let projects = scratch_projects("follow");
        let mut app = App::new(Config {
            projects_dir: projects.clone(),
            live: false,
            socket_dir: None,
            time: TimeStyle::default(),
            ago_within: DEFAULT_AGO_WITHIN,
        });
        app.go_to(Page::Tools);
        app.move_by(3);
        app.offsets[1] = 2; // scrolled, as after moving through a long list
        let watched = app.selected_tool().unwrap().1.id.clone();
        assert_eq!(watched, "toolu_02");

        // A newer call arrives at the top of the list.
        append_tool_call(&projects, "toolu_new1", 0);
        app.refresh();
        assert_eq!(app.tools().len(), 6);
        assert_eq!(app.tools()[0].1.id, "toolu_new1");
        // The cursor is still on its row, one further down, and the list
        // scrolled with it, so the row has not moved on screen.
        assert_eq!(app.selected_tool().unwrap().1.id, watched);
        assert_eq!((app.tool_selected, app.offsets[1]), (4, 3));

        // A cursor on the first row keeps following the latest.
        app.move_first();
        app.offsets[1] = 0;
        append_tool_call(&projects, "toolu_new2", 1);
        app.refresh();
        assert_eq!(app.tool_selected, 0);
        assert_eq!(app.selected_tool().unwrap().1.id, "toolu_new2");

        // Nothing new: nothing moves.
        app.move_by(2);
        let before = (app.tool_selected, app.offsets[1]);
        app.refresh();
        assert_eq!((app.tool_selected, app.offsets[1]), before);

        let _ = std::fs::remove_dir_all(projects);
    }

    #[test]
    fn entering_a_subagent_narrows_the_tool_calls_to_it() {
        let mut app = fixture_app();
        app.go_to(Page::Subagents);
        app.move_by(1);
        let agent = app.selected_subagent().unwrap().1.agent_id.clone();
        app.enter();
        assert_eq!(app.page, Page::Tools);
        assert_eq!(app.scope.as_deref(), Some(WITH_SUBAGENTS));
        assert_eq!(app.agent_scope.as_deref(), Some(agent.as_str()));
        assert_eq!(app.scoped_subagent().unwrap().agent_id, agent);
        // Its one call, and the detail pane is reachable from there.
        let calls = app.tools();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.agent_id.as_deref(), Some(agent.as_str()));
        app.enter();
        assert!(app.in_detail);
        app.back();

        // The Subagents page still lists the session's subagents, and its
        // cursor is still on the one entered.
        app.go_to(Page::Subagents);
        assert_eq!(app.subagents().len(), 2);
        assert_eq!(app.selected_subagent().unwrap().1.agent_id, agent);

        // Esc: first the subagent, then the session.
        app.go_to(Page::Tools);
        app.back();
        assert_eq!(
            (app.scope.is_some(), app.agent_scope.is_some()),
            (true, false)
        );
        assert_eq!(app.tools().len(), 5);
        app.back();
        assert_eq!(app.scope, None);

        // Entering a session forgets a subagent of another.
        app.go_to(Page::Subagents);
        app.enter();
        assert!(app.agent_scope.is_some());
        app.go_to(Page::Sessions);
        app.move_last();
        app.enter();
        assert_eq!(
            (app.scope.as_deref(), app.agent_scope.clone()),
            (Some(INDEXED), None)
        );
    }

    #[test]
    fn enter_elsewhere_and_i_go_into_the_detail_pane() {
        let mut app = fixture_app();
        app.focus_detail();
        assert!(app.in_detail);
        // Enter in the detail pane does not narrow anything.
        app.enter();
        assert_eq!((app.page, app.scope.clone()), (Page::Sessions, None));
        app.back();
        assert!(!app.in_detail);

        app.go_to(Page::Tools);
        app.enter();
        assert!(app.in_detail);
        app.leave_detail();
        app.toggle_focus();
        assert!(app.in_detail);
        // Changing page starts on its list.
        app.go_to(Page::Subagents);
        assert!(!app.in_detail);
        // The Usage page has no panes to move between.
        app.go_to(Page::Usage);
        app.toggle_focus();
        app.focus_detail();
        assert!(!app.in_detail);
    }

    #[test]
    fn a_missing_projects_directory_is_a_notice() {
        let app = App::new(Config {
            projects_dir: fixtures().join("no-such-directory"),
            live: false,
            socket_dir: None,
            time: TimeStyle::default(),
            ago_within: DEFAULT_AGO_WITHIN,
        });
        assert!(app.order.is_empty());
        assert!(app.selected_session().is_none());
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("no projects directory"));
    }

    #[test]
    fn status_comes_from_the_transcript_without_live_events() {
        let app = fixture_app();
        let session = app.store.get(INDEXED).unwrap();
        let just_after = Timestamp::parse("2026-11-01T09:00:10.000Z").unwrap();
        let much_later = Timestamp::parse("2026-11-09T09:00:00.000Z").unwrap();
        assert_eq!(status(session, just_after).text, "active");
        assert_eq!(status(session, just_after).kind, StatusKind::Busy);
        assert_eq!(status(session, much_later).kind, StatusKind::Idle);
    }

    #[cfg(unix)]
    #[test]
    fn live_events_reach_the_store_through_the_socket() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let socket_dir = std::env::temp_dir().join(format!("ccs-tui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&socket_dir);
        let mut app = App::new(Config {
            projects_dir: fixtures().join(KNOWN_CLAUDE_CODE_VERSION).join("projects"),
            live: true,
            socket_dir: Some(socket_dir.clone()),
            time: TimeStyle::default(),
            ago_within: DEFAULT_AGO_WITHIN,
        });
        let LiveStatus::Listening(socket) = app.live.clone() else {
            panic!("{:?}", app.live)
        };

        for name in ["turn-start", "tool-check-ask"] {
            let body =
                std::fs::read_to_string(fixtures().join("events").join(format!("{name}.json")))
                    .unwrap();
            let mut stream = UnixStream::connect(&socket).unwrap();
            write!(
                stream,
                "POST /events HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        }

        app.refresh();
        let session = app.store.get(WITH_SUBAGENTS).unwrap();
        let shown = status(session, now());
        assert_eq!(shown.kind, StatusKind::NeedsApproval);
        assert_eq!(shown.text, "approve Write?");
        // The transcript side is untouched.
        assert_eq!(session.usage.total().responses, 7);

        drop(app);
        let _ = std::fs::remove_dir_all(socket_dir);
    }
}

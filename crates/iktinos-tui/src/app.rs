//! The viewer's state: a `SessionStore` kept current from the transcripts
//! and, when the bridge mod is installed, from live events.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iktinos::discover::{discover, SessionFiles};
use iktinos::record::{Record, Timestamp};
use iktinos::session::{Activity, LiveState, Session, SessionStore, Subagent, ToolCall};
use iktinos::tail::Tail;

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
    /// Working: a turn in progress, a tool running, or a fresh record.
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
    Usage,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Sessions, Page::Tools, Page::Subagents, Page::Usage];

    pub fn title(self) -> &'static str {
        match self {
            Page::Sessions => "Sessions",
            Page::Tools => "Tool calls",
            Page::Subagents => "Subagents",
            Page::Usage => "Usage",
        }
    }

    /// Whether the page is a list over a detail pane.
    pub fn has_list(self) -> bool {
        self != Page::Usage
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
    /// The ids of the sessions the Sessions page lists, in display order.
    pub order: Vec<String>,
    /// The session the Tool calls and Subagents pages are narrowed to.
    pub scope: Option<String>,
    pub selected: usize,
    pub tool_selected: usize,
    pub subagent_selected: usize,
    pub detail_scroll: usize,
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

impl App {
    pub fn new(config: Config) -> Self {
        let mut app = Self {
            store: SessionStore::new().with_recent_tools(RECENT_TOOLS),
            tails: BTreeMap::new(),
            page: Page::Sessions,
            in_detail: false,
            sort: Sort::Updated,
            filter: Filter::All,
            order: Vec::new(),
            scope: None,
            selected: 0,
            tool_selected: 0,
            subagent_selected: 0,
            detail_scroll: 0,
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
            self.detail_scroll = 0;
        }
        self.selected = kept.unwrap_or(0);
        if self
            .scope
            .as_ref()
            .is_some_and(|id| self.store.get(id).is_none())
        {
            self.scope = None;
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
            .collect();
        calls.sort_by(|a, b| {
            b.1.started_at
                .cmp(&a.1.started_at)
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
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

    pub fn selected_tool(&self) -> Option<(&Session, &ToolCall)> {
        self.tools().get(self.tool_selected).copied()
    }

    pub fn selected_subagent(&self) -> Option<(&Session, &Subagent)> {
        self.subagents().get(self.subagent_selected).copied()
    }

    fn list_len(&self, page: Page) -> usize {
        match page {
            Page::Sessions => self.order.len(),
            Page::Tools => self.tools().len(),
            Page::Subagents => self.subagents().len(),
            Page::Usage => 0,
        }
    }

    fn cursor_mut(&mut self, page: Page) -> &mut usize {
        match page {
            Page::Sessions => &mut self.selected,
            Page::Tools => &mut self.tool_selected,
            Page::Subagents | Page::Usage => &mut self.subagent_selected,
        }
    }

    fn clamp(&mut self) {
        for page in [Page::Sessions, Page::Tools, Page::Subagents] {
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
            self.detail_scroll = 0;
        }
    }

    pub fn move_by(&mut self, by: isize) {
        let at = if self.in_detail {
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
        let rows = if self.in_detail {
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
            self.detail_scroll = 0;
        }
    }

    /// `Tab` / `Shift+Tab`: the page's two panes.
    pub fn toggle_focus(&mut self) {
        if self.page.has_list() {
            self.in_detail = !self.in_detail;
        }
    }

    /// `i`: into the detail pane.
    pub fn focus_detail(&mut self) {
        if self.page.has_list() {
            self.in_detail = true;
        }
    }

    /// `Enter`. On a session: narrow the Tool calls and Subagents pages to it
    /// and show its tool calls. Elsewhere: into the detail pane.
    pub fn enter(&mut self) {
        if self.page != Page::Sessions || self.in_detail {
            self.focus_detail();
            return;
        }
        let Some(id) = self.order.get(self.selected).cloned() else {
            return;
        };
        self.set_scope(Some(id));
        self.go_to(Page::Tools);
    }

    fn set_scope(&mut self, scope: Option<String>) {
        if self.scope != scope {
            self.scope = scope;
            self.tool_selected = 0;
            self.subagent_selected = 0;
            if self.page != Page::Sessions {
                self.detail_scroll = 0;
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
    }

    /// `h` / `Esc`: out of the detail pane. On a list `Esc` drops the search,
    /// and with no search the narrowing to a session.
    pub fn back(&mut self) {
        if self.in_detail {
            self.in_detail = false;
        } else if self.search.as_ref().is_some_and(|s| s.page == self.page) {
            self.search = None;
        } else {
            self.set_scope(None);
        }
    }

    /// `h`: out of the detail pane only.
    pub fn leave_detail(&mut self) {
        self.in_detail = false;
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

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
    Usage,
}

/// The panes of the Sessions page, in `Tab` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sessions,
    Tools,
    Subagents,
    Detail,
}

impl Pane {
    const ORDER: [Pane; 4] = [Pane::Sessions, Pane::Tools, Pane::Subagents, Pane::Detail];

    fn step(self, by: isize) -> Pane {
        let at = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0) as isize;
        Self::ORDER[(at + by).rem_euclid(Self::ORDER.len() as isize) as usize]
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

/// A search within one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub pane: Pane,
    /// Lower-cased: matching ignores case.
    pub query: String,
}

/// What the last draw measured, for keys that move by what is visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Viewport {
    /// Rows a list pane shows.
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
    pub focus: Pane,
    /// The list the detail pane describes, and that `Esc` returns to.
    pub detail_of: Pane,
    pub sort: Sort,
    pub filter: Filter,
    /// The ids of the sessions shown, in display order.
    pub order: Vec<String>,
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

/// How many recent tool calls per session the viewer keeps to show.
const RECENT_TOOLS: usize = 200;

impl App {
    pub fn new(config: Config) -> Self {
        let mut app = Self {
            store: SessionStore::new().with_recent_tools(RECENT_TOOLS),
            tails: BTreeMap::new(),
            page: Page::Sessions,
            focus: Pane::Sessions,
            detail_of: Pane::Sessions,
            sort: Sort::Updated,
            filter: Filter::All,
            order: Vec::new(),
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
        if kept.is_none() {
            // Another session is under the cursor now.
            self.tool_selected = 0;
            self.subagent_selected = 0;
            self.detail_scroll = 0;
        }
        self.selected = kept.unwrap_or(0).min(self.order.len().saturating_sub(1));
        self.clamp();
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.order
            .get(self.selected)
            .and_then(|id| self.store.get(id))
    }

    /// The selected session's recent tool calls, latest first.
    pub fn tools(&self) -> Vec<&ToolCall> {
        let mut calls: Vec<&ToolCall> = self
            .selected_session()
            .map(|s| s.recent_tools.iter().collect())
            .unwrap_or_default();
        // The store keeps them in the order they were read.
        calls.sort_by_key(|call| std::cmp::Reverse(call.started_at));
        calls
    }

    /// The selected session's subagents, the latest active first.
    pub fn subagents(&self) -> Vec<&Subagent> {
        let mut subagents: Vec<&Subagent> = self
            .selected_session()
            .map(|s| s.subagents.values().collect())
            .unwrap_or_default();
        subagents.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.agent_id.cmp(&b.agent_id))
        });
        subagents
    }

    pub fn selected_tool(&self) -> Option<&ToolCall> {
        self.tools().get(self.tool_selected).copied()
    }

    pub fn selected_subagent(&self) -> Option<&Subagent> {
        self.subagents().get(self.subagent_selected).copied()
    }

    /// How many rows the focused list holds.
    fn len_of(&self, pane: Pane) -> usize {
        match pane {
            Pane::Sessions => self.order.len(),
            Pane::Tools => self.tools().len(),
            Pane::Subagents => self.subagents().len(),
            Pane::Detail => self.viewport.detail_lines,
        }
    }

    fn cursor_mut(&mut self, pane: Pane) -> &mut usize {
        match pane {
            Pane::Sessions => &mut self.selected,
            Pane::Tools => &mut self.tool_selected,
            Pane::Subagents => &mut self.subagent_selected,
            Pane::Detail => &mut self.detail_scroll,
        }
    }

    fn clamp(&mut self) {
        for pane in [Pane::Sessions, Pane::Tools, Pane::Subagents] {
            let last = self.len_of(pane).saturating_sub(1);
            let cursor = self.cursor_mut(pane);
            *cursor = (*cursor).min(last);
        }
    }

    /// Moves the focused pane's cursor (or scrolls the detail pane) to `to`,
    /// clamped to what the pane holds.
    fn move_to(&mut self, to: usize) {
        let pane = self.focus;
        let last = match pane {
            Pane::Detail => self
                .viewport
                .detail_lines
                .saturating_sub(self.viewport.detail_rows),
            _ => self.len_of(pane).saturating_sub(1),
        };
        let to = to.min(last);
        if *self.cursor_mut(pane) == to {
            return;
        }
        *self.cursor_mut(pane) = to;
        match pane {
            Pane::Sessions => {
                self.tool_selected = 0;
                self.subagent_selected = 0;
                self.detail_scroll = 0;
            }
            Pane::Tools | Pane::Subagents => self.detail_scroll = 0,
            Pane::Detail => {}
        }
    }

    pub fn move_by(&mut self, by: isize) {
        let at = *self.cursor_mut(self.focus);
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
        let rows = match self.focus {
            Pane::Detail => self.viewport.detail_rows,
            _ => self.viewport.list_rows,
        };
        (rows / 2).max(1) as isize
    }

    pub fn focus(&mut self, pane: Pane) {
        if pane != Pane::Detail && pane != self.detail_of {
            self.detail_of = pane;
            self.detail_scroll = 0;
        }
        self.focus = pane;
    }

    /// `Tab` / `Shift+Tab`.
    pub fn cycle_focus(&mut self, by: isize) {
        self.focus(self.focus.step(by));
    }

    /// `h` / `l` among the upper panes; `h` leaves the detail pane.
    pub fn focus_sideways(&mut self, by: isize) {
        match self.focus {
            Pane::Detail if by < 0 => self.focus(self.detail_of),
            Pane::Detail => {}
            pane => {
                let upper = [Pane::Sessions, Pane::Tools, Pane::Subagents];
                let at = upper.iter().position(|p| *p == pane).unwrap_or(0) as isize;
                let to = (at + by).clamp(0, upper.len() as isize - 1) as usize;
                self.focus(upper[to]);
            }
        }
    }

    /// `Esc`: out of the detail pane, else drop the search.
    pub fn back(&mut self) {
        if self.focus == Pane::Detail {
            self.focus(self.detail_of);
        } else {
            self.search = None;
        }
    }

    pub fn cycle_sort(&mut self) {
        self.sort = match self.sort {
            Sort::Updated => Sort::Cost,
            Sort::Cost => Sort::Tokens,
            Sort::Tokens => Sort::Updated,
        };
        self.arrange(now());
    }

    pub fn cycle_filter(&mut self) {
        self.filter = match self.filter {
            Filter::All => Filter::Live,
            Filter::Live => Filter::Open,
            Filter::Open => Filter::All,
        };
        self.arrange(now());
    }

    /// The text a row of `pane` is searched by, lower-cased.
    fn haystacks(&self, pane: Pane, now: Timestamp) -> Vec<String> {
        let rows: Vec<String> = match pane {
            Pane::Sessions => self
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
            Pane::Tools => self
                .tools()
                .iter()
                .map(|c| format!("{} {}", c.name, c.summary.as_deref().unwrap_or_default()))
                .collect(),
            Pane::Subagents => self
                .subagents()
                .iter()
                .map(|a| {
                    format!(
                        "{} {} {}",
                        a.agent_type.as_deref().unwrap_or_default(),
                        a.description.as_deref().unwrap_or_default(),
                        a.agent_id
                    )
                })
                .collect(),
            Pane::Detail => Vec::new(),
        };
        rows.into_iter().map(|row| row.to_lowercase()).collect()
    }

    /// The rows of `pane` the search in force matches.
    pub fn matches(&self, pane: Pane, now: Timestamp) -> Vec<usize> {
        let Some(search) = self.search.as_ref().filter(|s| s.pane == pane) else {
            return Vec::new();
        };
        self.haystacks(pane, now)
            .iter()
            .enumerate()
            .filter(|(_, row)| row.contains(&search.query))
            .map(|(i, _)| i)
            .collect()
    }

    /// Starts typing a search in the focused list.
    pub fn start_search(&mut self) {
        if self.focus != Pane::Detail {
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
        self.search = Some(Search {
            pane: self.focus,
            query,
        });
        let now = now();
        let at = *self.cursor_mut(self.focus);
        let matches = self.matches(self.focus, now);
        if let Some(to) = matches.iter().find(|m| **m >= at).or(matches.first()) {
            self.move_to(*to);
        }
    }

    /// `n` / `N`: the next or the previous match, wrapping around.
    pub fn next_match(&mut self, by: isize) {
        let Some(pane) = self.search.as_ref().map(|s| s.pane) else {
            return;
        };
        if pane != self.focus {
            return;
        }
        let matches = self.matches(pane, now());
        let at = *self.cursor_mut(pane);
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
    fn the_lower_lists_belong_to_the_selected_session() {
        let mut app = fixture_app();
        // The first session (the damaged transcript) has neither.
        assert!(app.tools().is_empty());
        assert!(app.subagents().is_empty());
        assert!(app.selected_tool().is_none());

        app.move_by(1);
        let names: Vec<_> = app.tools().iter().map(|c| c.name.as_str()).collect();
        // The latest first; the main thread's and the subagents' together.
        assert_eq!(names.len(), 5);
        assert_eq!(names[4], "Bash");
        assert_eq!(names[3], "Read");
        let agents: Vec<_> = app
            .subagents()
            .iter()
            .map(|a| a.agent_id.as_str())
            .collect();
        assert_eq!(agents, ["a1b2c3d4e5f60718", "b2c3d4e5f6071829"]);

        // Each list has its own cursor, and moving on to another session
        // starts them again.
        app.focus(Pane::Tools);
        app.move_by(3);
        assert_eq!(app.selected_tool().unwrap().name, "Read");
        app.focus(Pane::Subagents);
        app.move_by(1);
        assert_eq!(
            app.selected_subagent().unwrap().agent_id,
            "b2c3d4e5f6071829"
        );
        app.focus(Pane::Sessions);
        assert_eq!(app.tool_selected, 3, "changing pane keeps the cursors");
        app.move_by(1);
        assert_eq!((app.tool_selected, app.subagent_selected), (0, 0));
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

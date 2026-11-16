//! The viewer's state: a `SessionStore` kept current from the transcripts
//! and, when the bridge mod is installed, from live events.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iktinos::discover::{discover, SessionFiles};
use iktinos::record::{Record, Timestamp};
use iktinos::session::{Activity, LiveState, Session, SessionStore};
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

pub struct App {
    config: Config,
    pub store: SessionStore,
    /// A tail per transcript, with the session it belongs to.
    tails: BTreeMap<PathBuf, (String, Tail)>,
    /// Session ids in display order: latest activity first.
    pub order: Vec<String>,
    pub selected: usize,
    /// Something the user should know: a missing directory, a failed socket.
    pub notice: Option<String>,
    pub live: LiveStatus,
    #[cfg(unix)]
    listener: Option<live::Listener>,
}

impl App {
    pub fn new(config: Config) -> Self {
        let mut app = Self {
            store: SessionStore::new(),
            tails: BTreeMap::new(),
            order: Vec::new(),
            selected: 0,
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
        self.sort();
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

    fn sort(&mut self) {
        let selected = self.order.get(self.selected).cloned();
        let mut sessions: Vec<&Session> = self.store.sessions().collect();
        sessions.sort_by(|a, b| latest(b).cmp(&latest(a)).then_with(|| a.id.cmp(&b.id)));
        self.order = sessions.into_iter().map(|s| s.id.clone()).collect();
        // Keep the cursor on the same session while the list reorders.
        self.selected = selected
            .and_then(|id| self.order.iter().position(|other| *other == id))
            .unwrap_or(0)
            .min(self.order.len().saturating_sub(1));
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.order
            .get(self.selected)
            .and_then(|id| self.store.get(id))
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.order.len() {
            self.selected += 1;
        }
    }

    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.order.len().saturating_sub(1);
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
        app.select_previous();
        assert_eq!(app.selected, 0);
        app.select_last();
        assert_eq!(app.selected, 2);
        app.select_next();
        assert_eq!(app.selected, 2);
        app.select_first();
        app.select_next();
        assert_eq!(app.selected_session().unwrap().id, WITH_SUBAGENTS);
        // The list is re-sorted on refresh; the cursor stays on its session.
        app.refresh();
        assert_eq!(app.selected_session().unwrap().id, WITH_SUBAGENTS);
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

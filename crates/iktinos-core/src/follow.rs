//! Keeping a [`SessionStore`] current: the loop every consumer runs.
//!
//! [`discover`] finds the sessions, a [`Tail`] per transcript reads what was
//! appended, the store folds it, and a transcript that was replaced makes its
//! session start over. A [`Follower`] is that loop, with the state it needs
//! between two turns of it:
//!
//! ```no_run
//! use iktinos_core::discover::default_projects_dir;
//! use iktinos_core::follow::Follower;
//! use iktinos_core::session::SessionStore;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut store = SessionStore::new();
//! let mut follower = Follower::new(default_projects_dir().ok_or("no home directory")?);
//! loop {
//!     // Reads only what is new since the last call.
//!     follower.poll(&mut store)?;
//!     for session in store.sessions() {
//!         println!("{} {:?}", session.label(), session.cost.usd);
//!     }
//!     std::thread::sleep(std::time::Duration::from_secs(1));
//! }
//! # }
//! ```
//!
//! Live events are not its business: a consumer that listens for them
//! applies them to the same store between two polls.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::discover::{discover, DiscoverError, SessionFiles};
use crate::record::Record;
use crate::session::SessionStore;
use crate::tail::Tail;

/// The transcripts under one projects directory, and how far each was read.
#[derive(Debug, Clone)]
pub struct Follower {
    projects_dir: PathBuf,
    /// A tail per transcript, with the session it belongs to.
    tails: BTreeMap<PathBuf, (String, Tail)>,
    /// The files of every session seen, by session id.
    files: BTreeMap<String, SessionFiles>,
}

/// What one [`Follower::poll`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Followed {
    /// Records folded into the store.
    pub records: usize,
    /// Sessions that started over because one of their transcripts was
    /// replaced: everything folded from them before was forgotten and read
    /// again.
    pub restarted: Vec<String>,
}

impl Follower {
    /// Follows the transcripts under `projects_dir`. Nothing is read until
    /// the first [`poll`](Self::poll).
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self {
            projects_dir: projects_dir.into(),
            tails: BTreeMap::new(),
            files: BTreeMap::new(),
        }
    }

    /// The directory that is followed.
    pub fn projects_dir(&self) -> &Path {
        &self.projects_dir
    }

    /// Looks for new sessions and subagents, then folds into `store` what
    /// was appended to every transcript since the last call; the first call
    /// reads everything.
    ///
    /// When the directory cannot be listed, what is already known is still
    /// read, and the error is returned: a directory that is missing for a
    /// moment does not stop the sessions from being followed.
    pub fn poll(&mut self, store: &mut SessionStore) -> Result<Followed, DiscoverError> {
        let found = discover(&self.projects_dir);
        if let Ok(found) = &found {
            for files in found {
                self.track(store, files);
            }
        }
        let followed = self.read_appended(store);
        found.map(|_| followed)
    }

    /// The files of a session that was seen: where its transcripts are, for
    /// reading something of it again (a tool call's whole output).
    pub fn files(&self, session_id: &str) -> Option<&SessionFiles> {
        self.files.get(session_id)
    }

    fn track(&mut self, store: &mut SessionStore, files: &SessionFiles) {
        store.discovered(files);
        self.files.insert(files.id.clone(), files.clone());
        let transcripts = std::iter::once(&files.transcript)
            .chain(files.subagents.iter().map(|subagent| &subagent.transcript));
        for transcript in transcripts {
            self.tails
                .entry(transcript.path.clone())
                .or_insert_with(|| (files.id.clone(), Tail::from_start(&transcript.path)));
        }
    }

    fn read_appended(&mut self, store: &mut SessionStore) -> Followed {
        let mut followed = Followed::default();
        for (id, tail) in self.tails.values_mut() {
            let polled = tail.poll_each(|line, _| store.apply(id, &Record::parse_bytes(line)));
            // A transcript that vanished or cannot be read is skipped; it is
            // tried again on the next poll.
            let Ok(polled) = polled else {
                continue;
            };
            followed.records += polled.lines;
            if polled.restarted && !followed.restarted.contains(id) {
                followed.restarted.push(id.clone());
            }
        }
        // A replaced file invalidates what was folded from it: forget the
        // session and read all of its files again.
        for id in &followed.restarted {
            store.reset(id);
            for (owner, tail) in self.tails.values_mut() {
                if owner == id {
                    *tail = Tail::from_start(tail.path());
                    let _ = tail.poll_each(|line, _| store.apply(id, &Record::parse_bytes(line)));
                }
            }
        }
        followed
    }
}

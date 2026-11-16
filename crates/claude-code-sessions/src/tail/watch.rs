//! Filesystem notifications instead of polling on a timer (the `watch`
//! feature).
//!
//! A [`Watcher`] watches a projects directory and sends the path of every
//! transcript that was created or written to. The caller polls the
//! [`Tail`](super::Tail) of that path; the watcher reads nothing itself.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use notify::{EventKind, RecursiveMode, Watcher as _};

/// Watches a directory tree for transcript changes until dropped.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
}

impl Watcher {
    /// Starts watching `projects_dir` and everything below it. The receiver
    /// yields the path of each `*.jsonl` file that was created or modified;
    /// one append may be reported more than once.
    pub fn new(projects_dir: &Path) -> Result<(Self, Receiver<PathBuf>), WatchError> {
        let (sender, receiver) = mpsc::channel();
        let mut inner =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    return;
                };
                if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    return;
                }
                for path in event.paths {
                    if path.extension().is_some_and(|e| e == "jsonl") {
                        // The receiver being gone only means nobody listens.
                        let _ = sender.send(path);
                    }
                }
            })?;
        inner.watch(projects_dir, RecursiveMode::Recursive)?;
        Ok((Self { _inner: inner }, receiver))
    }
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

/// The directory could not be watched.
#[derive(Debug, thiserror::Error)]
#[error("cannot watch for transcript changes: {0}")]
pub struct WatchError(#[from] notify::Error);

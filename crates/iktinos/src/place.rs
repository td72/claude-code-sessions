//! Where a session runs, compared with a directory: is it there, below it,
//! or in another worktree of the same git repository?
//!
//! A viewer started inside a repository usually wants that repository's
//! sessions only. [`Place`] answers for one directory, chosen once; the two
//! questions it asks are plain functions, [`is_within`] and
//! [`git_common_dir`].
//!
//! Two worktrees belong to the same repository when they share a common git
//! directory: the `.git` directory of the main worktree, which a linked
//! worktree's `.git` *file* points to through `gitdir:` and, at the end of
//! that, `commondir`. No `git` process is started. This is the one place the
//! crate reads outside Claude Code's directories, and it reads no more than
//! that: whether `.git` is there, and the two short files. Nothing is
//! written.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most that is read of a `.git` file or a `commondir`: they hold one
/// path.
const MAX_POINTER_BYTES: u64 = 4096;

/// Whether `path` is `dir` or below it, by whole components: `/work/foo` is
/// not within `/work/foobar`, nor the other way round. Neither is resolved
/// here.
pub fn is_within(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}

/// The common git directory of the repository `path` is in: the nearest
/// `.git` at or above it, followed where it is the file of a linked
/// worktree. `None` outside a repository, and where the `.git` that was
/// found does not lead anywhere.
///
/// `path` need not exist: the search starts at its nearest ancestor that
/// does.
pub fn git_common_dir(path: &Path) -> Option<PathBuf> {
    for dir in path.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return fs::canonicalize(dot_git).ok();
        }
        // A linked worktree (or a submodule): `gitdir: <its git directory>`.
        let pointer = read_pointer(&dot_git)?;
        let git_dir = dir.join(pointer.strip_prefix("gitdir:")?.trim());
        // A worktree's git directory names the one all worktrees share. One
        // without `commondir` is its own.
        let common = match read_pointer(&git_dir.join("commondir")) {
            Some(common) => git_dir.join(common),
            None => git_dir,
        };
        return fs::canonicalize(common).ok();
    }
    None
}

/// The first line of a small file, trimmed. `None` if it cannot be read or
/// is empty.
fn read_pointer(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(MAX_POINTER_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// A directory, and the answer to "does this session belong here?".
#[derive(Debug, Clone)]
pub struct Place {
    dir: PathBuf,
    common: Option<PathBuf>,
    /// The answers given so far: a session's location is asked about again
    /// and again.
    known: HashMap<PathBuf, bool>,
}

impl Place {
    /// `dir` is resolved once, here. One that cannot be resolved (it does
    /// not exist) is taken as it is.
    pub fn new(dir: &Path) -> Self {
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
        let common = git_common_dir(&dir);
        Self {
            dir,
            common,
            known: HashMap::new(),
        }
    }

    /// The directory, resolved.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `path` is the directory, or below it, or inside a worktree
    /// of the same git repository. A path that no longer exists is compared
    /// as it is written.
    pub fn holds(&mut self, path: &Path) -> bool {
        if let Some(known) = self.known.get(path) {
            return *known;
        }
        let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let holds = is_within(&resolved, &self.dir)
            || self
                .common
                .as_ref()
                .is_some_and(|common| git_common_dir(&resolved).as_ref() == Some(common));
        self.known.insert(path.to_owned(), holds);
        holds
    }
}

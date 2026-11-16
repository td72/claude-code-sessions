//! Sessions and subagents under a projects directory.
//!
//! Claude Code keeps one directory per project under `~/.claude/projects`,
//! named after the project path with `/` and `.` both flattened to `-` — a
//! name that cannot be decoded back into a path. Inside it:
//!
//! ```text
//! <project>/sessions-index.json                       (not always present)
//! <project>/<session id>.jsonl
//! <project>/<session id>/subagents/agent-<id>.jsonl
//! <project>/<session id>/subagents/agent-<id>.meta.json
//! ```
//!
//! [`discover`] lists every session with its subagents, file sizes and
//! modification times, and resolves each session's working directory: from
//! `sessions-index.json` when the index lists the session, otherwise from the
//! first record of the transcript that carries a `cwd`.
//!
//! Only the projects directory itself can fail the call. A project directory
//! or file that cannot be read is left out.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::record::Record;

/// How much of a transcript is read to find its `cwd`. The first record that
/// carries one sits within the first few lines.
const CWD_SCAN_BYTES: u64 = 256 * 1024;

/// The projects directory Claude Code uses: `$CLAUDE_CONFIG_DIR/projects`, or
/// `~/.claude/projects`. `None` when neither variable is set.
pub fn default_projects_dir() -> Option<PathBuf> {
    let config = match env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var_os("HOME")?).join(".claude"),
    };
    Some(config.join("projects"))
}

/// Lists every session under `projects_dir`, ordered by transcript path.
pub fn discover(projects_dir: &Path) -> Result<Vec<SessionFiles>, DiscoverError> {
    let entries = fs::read_dir(projects_dir).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            DiscoverError::Missing(projects_dir.to_owned())
        } else {
            DiscoverError::Unreadable {
                path: projects_dir.to_owned(),
                source,
            }
        }
    })?;

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let project_dir = entry.path();
        if project_dir.is_dir() {
            discover_project(&project_dir, &mut sessions);
        }
    }
    sessions.sort_by(|a, b| a.transcript.path.cmp(&b.transcript.path));
    Ok(sessions)
}

fn discover_project(project_dir: &Path, sessions: &mut Vec<SessionFiles>) {
    let Ok(entries) = fs::read_dir(project_dir) else {
        return;
    };
    let index = Index::load(project_dir);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let Some(transcript) = TranscriptFile::stat(path) else {
            continue;
        };
        let (cwd, cwd_source) = match index.as_ref().and_then(|i| i.listed.get(&id)) {
            Some(listed) => (Some(listed.clone()), Some(CwdSource::Index)),
            None => match cwd_from_transcript(&transcript.path) {
                Some(cwd) => (Some(cwd), Some(CwdSource::Transcript)),
                // Nothing in the transcript yet: the project's own path is
                // the best remaining answer.
                None => match index.as_ref().and_then(|i| i.original_path.clone()) {
                    Some(cwd) => (Some(cwd), Some(CwdSource::Index)),
                    None => (None, None),
                },
            },
        };
        sessions.push(SessionFiles {
            subagents: discover_subagents(&project_dir.join(&id).join("subagents")),
            id,
            project_dir: project_dir.to_owned(),
            transcript,
            cwd,
            cwd_source,
        });
    }
}

fn discover_subagents(dir: &Path) -> Vec<SubagentFiles> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut subagents = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Some(agent_id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("agent-"))
            .map(str::to_owned)
        else {
            continue;
        };
        let meta = SubagentMeta::load(&path.with_extension("meta.json"));
        let Some(transcript) = TranscriptFile::stat(path) else {
            continue;
        };
        subagents.push(SubagentFiles {
            agent_id,
            transcript,
            meta,
        });
    }
    subagents.sort_by(|a, b| a.transcript.path.cmp(&b.transcript.path));
    subagents
}

/// One session on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFiles {
    /// The session id: the transcript's file name without `.jsonl`.
    pub id: String,
    /// The project directory the transcript lives in.
    pub project_dir: PathBuf,
    pub transcript: TranscriptFile,
    /// The session's working directory, when it could be resolved.
    pub cwd: Option<PathBuf>,
    /// Where [`cwd`](Self::cwd) came from.
    pub cwd_source: Option<CwdSource>,
    /// The session's subagents, ordered by transcript path.
    pub subagents: Vec<SubagentFiles>,
}

/// Where a working directory was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdSource {
    /// `sessions-index.json`.
    Index,
    /// The first record of the transcript that carries a `cwd`.
    Transcript,
}

/// A transcript file and what the filesystem says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    pub path: PathBuf,
    /// Size in bytes when it was listed.
    pub len: u64,
    /// Last modification, where the filesystem reports one.
    pub modified: Option<SystemTime>,
}

impl TranscriptFile {
    fn stat(path: PathBuf) -> Option<Self> {
        let meta = fs::metadata(&path).ok()?;
        meta.is_file().then(|| Self {
            path,
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    /// Whether the file was modified at or after `since`. `false` when the
    /// modification time is unknown.
    pub fn modified_since(&self, since: SystemTime) -> bool {
        self.modified.is_some_and(|m| m >= since)
    }
}

/// One subagent of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentFiles {
    /// The id in `agent-<id>.jsonl`, as records carry it in `agentId`.
    pub agent_id: String,
    pub transcript: TranscriptFile,
    /// `agent-<id>.meta.json`, when present and readable.
    pub meta: Option<SubagentMeta>,
}

/// What `agent-<id>.meta.json` says about a subagent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentMeta {
    /// `Explore`, `general-purpose`, …
    pub agent_type: Option<String>,
    /// The short task description the parent gave the agent.
    pub description: Option<String>,
    /// The parent's `tool_use` block that spawned the agent.
    pub tool_use_id: Option<String>,
    /// How deep the agent is: 1 for an agent the session spawned itself.
    pub spawn_depth: Option<u64>,
    /// Absent while the agent is still running.
    pub finished: Option<bool>,
}

impl SubagentMeta {
    fn load(path: &Path) -> Option<Self> {
        let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        let obj = value.as_object()?;
        let string = |key: &str| obj.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            agent_type: string("agentType"),
            description: string("description"),
            tool_use_id: string("toolUseId"),
            spawn_depth: obj.get("spawnDepth").and_then(Value::as_u64),
            finished: obj.get("finished").and_then(Value::as_bool),
        })
    }
}

/// The projects directory could not be listed.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    /// There is no such directory: Claude Code has not run here, or the path
    /// is wrong.
    #[error("no projects directory at {}", .0.display())]
    Missing(PathBuf),
    #[error("cannot read the projects directory {}: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What a project's `sessions-index.json` says about working directories.
struct Index {
    original_path: Option<PathBuf>,
    /// Session id → working directory, for the sessions the index lists.
    listed: HashMap<String, PathBuf>,
}

impl Index {
    fn load(project_dir: &Path) -> Option<Self> {
        let bytes = fs::read(project_dir.join("sessions-index.json")).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let path = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(PathBuf::from);
        let original_path = path(&value, "originalPath");
        let mut listed = HashMap::new();
        let entries = value.get("entries").and_then(Value::as_array);
        for entry in entries.into_iter().flatten() {
            let Some(id) = entry.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            if let Some(cwd) = path(entry, "projectPath").or_else(|| original_path.clone()) {
                listed.insert(id.to_owned(), cwd);
            }
        }
        Some(Self {
            original_path,
            listed,
        })
    }
}

/// The `cwd` of the first record that has one, looking no further than
/// [`CWD_SCAN_BYTES`] into the file.
fn cwd_from_transcript(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(CWD_SCAN_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        // A line cut off by the scan limit simply fails to parse.
        if let Some(cwd) = &Record::parse_bytes(&line).common().cwd {
            return Some(cwd.clone());
        }
    }
}

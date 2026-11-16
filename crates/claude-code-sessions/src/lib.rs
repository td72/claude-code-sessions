//! Claude Code sessions as data.
//!
//! Claude Code writes one JSON Lines transcript per session under
//! `~/.claude/projects/<project>/<session id>.jsonl` (subagents under
//! `<session id>/subagents/`). This crate turns those files into a
//! [`session::Session`] per session: where it runs, what it is doing, what it
//! has used, and what tools and subagents it has called. The format is
//! undocumented and changes between Claude Code releases, so every layer keeps
//! what it does not understand instead of failing on it.
//!
//! Two inputs feed one state. The transcripts are the source of truth for a
//! session's existence, its numbers (tokens, cost) and its history. The
//! optional `claude-code-sessions-live` crate adds freshness — the tool
//! running now, waiting for input, context fill, rate limits — from a Claude
//! Code mod. Both apply to a [`session::SessionStore`] incrementally; no field
//! is written by both, so arrival order never changes the result.
//!
//! Text is modelled where a view needs it (a title, the last prompt, a subagent's
//! description). The crates never log record contents themselves, and a session
//! keeps bounded recent activity, not its whole history.
//!
//! # Example
//!
//! ```no_run
//! use claude_code_sessions::discover::{default_projects_dir, discover};
//! use claude_code_sessions::record::Record;
//! use claude_code_sessions::session::SessionStore;
//! use claude_code_sessions::tail::Tail;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let projects = default_projects_dir().ok_or("no home directory")?;
//! let mut store = SessionStore::new();
//! let mut tails = Vec::new();
//!
//! for files in discover(&projects)? {
//!     store.discovered(&files);
//!     tails.push((files.id.clone(), Tail::from_start(&files.transcript.path)));
//!     for subagent in &files.subagents {
//!         tails.push((files.id.clone(), Tail::from_start(&subagent.transcript.path)));
//!     }
//! }
//!
//! // Call this again whenever the files may have grown: each poll reads only
//! // what was appended.
//! for (id, tail) in &mut tails {
//!     let polled = tail.poll_each(|line, _| store.apply(id, &Record::parse_bytes(line)))?;
//!     if polled.restarted {
//!         // The file was replaced: forget the session and read its files again.
//!         store.reset(id);
//!     }
//! }
//!
//! for session in store.sessions() {
//!     println!("{} {:?} {:?}", session.id, session.model, session.cost.usd);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! | module | role |
//! | --- | --- |
//! | [`record`] | one transcript line as a typed view, raw JSON kept, unknown shapes collected by `drift` |
//! | [`discover`] | sessions and subagents under a projects directory, `sessions-index.json`, cwd resolution |
//! | [`tail`] | offset-based line reading: load what is needed from the end, then follow appends |
//! | [`session`] | the fold of records into `Session` / `SessionStore` |
//! | [`pricing`] | the embedded price table and context windows |

pub mod record;

pub mod discover;

pub mod tail;

pub mod session;

pub mod pricing;

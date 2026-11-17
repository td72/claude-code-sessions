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
//! optional `iktinos-live` crate adds freshness — the tool
//! running now, waiting for input, context fill, rate limits — from a Claude
//! Code mod. Both apply to a [`session::SessionStore`] incrementally; no field
//! is written by both, so arrival order never changes the result.
//!
//! Text is modelled where a view needs it (a title, the last prompt, the first
//! line of each recent prompt and of each recent response, a subagent's
//! description, what a tool call was about and, in a store that asks for it,
//! the start of what the call returned). The crates never log record contents themselves, and a session
//! keeps bounded recent activity (its latest tool calls and interactions), not
//! its whole history. A tool call's whole output is read again from the
//! transcript when it is asked for, and kept nowhere.
//!
//! # Example
//!
//! ```no_run
//! use iktinos_core::discover::default_projects_dir;
//! use iktinos_core::follow::Follower;
//! use iktinos_core::session::SessionStore;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut store = SessionStore::new();
//! let mut follower = Follower::new(default_projects_dir().ok_or("no home directory")?);
//!
//! // Call this again whenever the files may have grown: the first poll
//! // reads everything, each one after it only what was appended.
//! follower.poll(&mut store)?;
//!
//! for session in store.sessions() {
//!     println!("{} {:?} {:?}", session.label(), session.model, session.cost.usd);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`follow::Follower`] is the loop every consumer runs. The parts it is
//! made of ([`discover`], [`tail::Tail`], [`session::SessionStore::apply`])
//! are there for a consumer that wants its own.
//!
//! | module | role |
//! | --- | --- |
//! | [`record`] | one transcript line as a typed view, raw JSON kept, unknown shapes collected by `drift` |
//! | [`discover`] | sessions and subagents under a projects directory, `sessions-index.json`, cwd resolution |
//! | [`tail`] | offset-based line reading: load what is needed from the end, then follow appends |
//! | [`session`] | the fold of records into `Session` / `SessionStore`, and what is derived from a session: its status, its name, where it runs |
//! | [`follow`] | the loop that keeps a `SessionStore` current: discover, tail, fold, start over |
//! | [`trace`] | a session's latest interactions as trees of spans: LLM requests, tool calls, subagents |
//! | [`output`] | the whole output of one tool call, and the whole text of one response, read again from the transcript on demand |
//! | [`place`] | where a session runs against a directory: below it, or in a worktree of the same git repository |
//! | [`pricing`] | the embedded price table and context windows |

pub mod record;

pub mod discover;

pub mod tail;

pub mod session;

pub mod follow;

pub mod place;

pub mod trace;

pub mod output;

pub mod pricing;

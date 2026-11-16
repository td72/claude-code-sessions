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
//! | module | role |
//! | --- | --- |
//! | [`record`] | one transcript line as a typed view, raw JSON kept, unknown shapes collected by `drift` |
//! | [`discover`] | sessions and subagents under a projects directory, `sessions-index.json`, cwd resolution |
//! | [`tail`] | offset-based line reading: load what is needed from the end, then follow appends |
//! | [`session`] | the fold of records into `Session` / `SessionStore` |
//! | [`pricing`] | the embedded price table and context windows |

pub mod record;

pub mod discover {
    //! Sessions and subagents under a projects directory, and the resolution
    //! of a session's working directory (`sessions-index.json` first, the
    //! first record's `cwd` otherwise — the directory name flattens `/` and
    //! `.` to `-` and cannot be decoded).
}

pub mod tail {
    //! Offset-based line reading. The initial load reads only what the fold
    //! needs from the end of a file; afterwards only appended bytes are read.
}

pub mod session {
    //! `Session` and `SessionStore`: the fold of records into state. Snapshot
    //! fields are last-write-wins by timestamp, accumulated fields (tokens,
    //! cost, recent activity) are folded once from the transcript offsets.
}

pub mod pricing {
    //! Price per token and context window per model, embedded as a snapshot.
    //! An unknown model prices as `None`, never as zero.
}

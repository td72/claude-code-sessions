//! Live events for `claude-code-sessions`.
//!
//! The transcripts lack, or report late, what a session is doing right now:
//! the tool running, waiting for input, context fill, rate limits. The
//! `claude-code-sessions-bridge` Claude Code mod (in this repository under
//! `mods/`) POSTs those as [`event::Event`]s to every Unix socket under the
//! shared socket directory, so several consumers (vig, the standalone viewer)
//! receive them at once.
//!
//! This crate is the receiving end only: it listens, parses a minimal
//! HTTP/1.1 `POST /events`, replies `200`, and never sends anything to Claude
//! Code. Events overwrite freshness fields of a `Session`; tokens, cost and
//! history stay with the transcripts, so nothing is counted twice.
//!
//! Unix only: the server is compiled out on other platforms and the crate then
//! exposes the protocol types alone.
//!
//! | module | role |
//! | --- | --- |
//! | [`event`] | the versioned (`v: 1`) event types; unknown types are ignored |
//! | [`server`] | the Unix-socket listener and the minimal HTTP parser, events out through an `mpsc` channel |
//! | [`merge`] | applying an event to a `Session` |

pub mod event;

#[cfg(unix)]
pub mod server;

pub mod merge;

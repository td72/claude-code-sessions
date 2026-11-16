# claude-code-sessions

Claude Code sessions as data.

Claude Code keeps one JSON Lines transcript per session under
`~/.claude/projects/`. This repository turns those files — and, optionally,
live events from a Claude Code mod — into a model of every session on the
machine: where it runs, what it is doing, what it has used, which tools and
subagents it has called. It is a library first; [vig](https://github.com/td72/vig)
renders it as a page, and a small standalone viewer ships here.

> Status: scaffolding. Nothing is implemented yet; the layout below is the
> design the crates are being built to.

## Crates

| crate | role |
| --- | --- |
| [`claude-code-sessions`](crates/claude-code-sessions) | core: `record` (typed transcript lines, raw kept, unknown shapes surveyed), `discover` (sessions, subagents, cwd resolution), `tail` (offset-based reading and append following), `session` (`Session` / `SessionStore`), `pricing` (embedded price table) |
| [`claude-code-sessions-live`](crates/claude-code-sessions-live) | the live-event protocol, the Unix-socket server that receives it, and the merge into `Session`. Listen only; never sends |
| [`claude-code-sessions-tui`](crates/claude-code-sessions-tui) | the `claude-code-sessions` binary: a small terminal viewer |
| [`mods/claude-code-sessions-bridge`](mods/claude-code-sessions-bridge) | the Claude Code mod that POSTs live events to every consumer's socket |

## Two inputs, one state

| | transcripts (JSONL) | mod (live events) |
| --- | --- | --- |
| setup | none | install the mod |
| past sessions | yes | no |
| latency | seconds, a turn at a time | immediate |
| owns | existence, tokens, cost, history, title, branch, cwd | running tool, waiting for input, context fill, rate limits |

Both feed a `SessionStore` incrementally. Snapshot fields are last-write-wins by
timestamp; accumulated fields are folded once from the transcript offsets. No
field is written by both inputs, so arrival order never changes the result.

Text is modelled where a view needs it (a title, the last prompt, a subagent's
description). The crates never log record contents themselves, and a session
keeps bounded recent activity, not its whole history.

## Why a crate

The popular tools in this space (ccusage, Claude-Code-Usage-Monitor,
claudelytics, ccboard) are applications, and the library crates on crates.io
are weeks old. The format is undocumented and moves with every Claude Code
release, so the parser keeps the raw record, treats unknown types and keys as
data for a drift report, and never fails a whole file on one bad line.

## License

MIT.

# claude-code-sessions

Claude Code sessions as data.

Claude Code keeps one JSON Lines transcript per session under
`~/.claude/projects/`. This repository turns those files — and, optionally,
live events from a Claude Code mod — into a model of every session on the
machine: where it runs, what it is doing, what it has used, which tools and
subagents it has called. It is a library first; [vig](https://github.com/td72/vig)
renders it as a page, and a small standalone viewer ships here.

> Status: the transcript layer (`claude-code-sessions`) is published. The live
> layer, the viewer and the mod are implemented here and not published yet.

## Using the crate

```rust
use claude_code_sessions::discover::{default_projects_dir, discover};
use claude_code_sessions::record::Record;
use claude_code_sessions::session::SessionStore;
use claude_code_sessions::tail::Tail;

let projects = default_projects_dir().ok_or("no home directory")?;
let mut store = SessionStore::new();
let mut tails = Vec::new();

for files in discover(&projects)? {
    store.discovered(&files);
    tails.push((files.id.clone(), Tail::from_start(&files.transcript.path)));
    for subagent in &files.subagents {
        tails.push((files.id.clone(), Tail::from_start(&subagent.transcript.path)));
    }
}

// Call this again whenever the files may have grown: each poll reads only
// what was appended.
for (id, tail) in &mut tails {
    let polled = tail.poll_each(|line, _| store.apply(id, &Record::parse_bytes(line)))?;
    if polled.restarted {
        // The file was replaced: forget the session and read its files again.
        store.reset(id);
    }
}

for session in store.sessions() {
    println!("{} {:?} {:?}", session.id, session.model, session.cost.usd);
}
```

`examples/sessions.rs` is the same loop with a table at the end, and
`examples/drift.rs` reports transcript shapes the crate does not know yet.

## Crates

| crate | role |
| --- | --- |
| [`claude-code-sessions`](crates/claude-code-sessions) | core: `record` (typed transcript lines, raw kept, unknown shapes surveyed), `discover` (sessions, subagents, cwd resolution), `tail` (offset-based reading and append following), `session` (`Session` / `SessionStore`), `pricing` (embedded price table) |
| [`claude-code-sessions-live`](crates/claude-code-sessions-live) | the live-event protocol, the Unix-socket server that receives it, and the merge into `Session`. Listen only; never sends |
| [`claude-code-sessions-tui`](crates/claude-code-sessions-tui) | the `claude-code-sessions` binary: a small terminal viewer |
| [`mods/sessions-bridge`](mods/sessions-bridge) | the Claude Code mod that POSTs live events to every consumer's socket |

## The viewer

```bash
cargo run -p claude-code-sessions-tui            # the sessions on this machine
cargo run -p claude-code-sessions-tui -- --projects-dir tests/fixtures/2.1.325/projects --no-live
```

A session list (title, repository, branch, state, last update, cost) and a
detail pane for the selected session: tokens and cost by model, the context
gauge, recent tool calls, subagents, rate limits. It reads the transcripts
once a second and, unless `--no-live` is given, listens for live events on its
own socket. A cost shown as `~$1.23` contains an estimate; `$1.23` is Claude
Code's own figure. It is a small development tool; vig renders the same model
as a full page.

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

## Cost

Claude Code writes its own running total into the transcript when it exits.
That total includes requests the transcript never records, so it is used where
it exists. But it stops at the last clean exit, and a process that was killed
leaves it far behind what the transcript shows. A session's cost is therefore
the larger of "reported total plus the price-table estimate of everything after
it" and "the estimate of everything", and it carries its source: reported,
reported plus estimate, or estimate. Token counts always come from the
transcript, one count per API response.

## Prices

`pricing` embeds a snapshot of Anthropic's first-party price list
(`crates/claude-code-sessions/assets/pricing.json`: input, output, 5 minute and
1 hour cache writes, cache reads and the context window per model, plus fast
mode and Claude Haiku 5.5's long-prompt prices). A model the table does not
know has no price; it is shown as `—`, never as `$0`.

To refresh the snapshot, copy the rows of the
[pricing page](https://platform.claude.com/docs/en/about-claude/pricing) into
the JSON, set `as_of`, and run `cargo test -p claude-code-sessions pricing`:
the tests reject a table whose rates break the published multipliers (cache
writes at 1.25× and 2× the input price, output at 5×). Automatic refresh is
future work.

## Why a crate

The popular tools in this space (ccusage, Claude-Code-Usage-Monitor,
claudelytics, ccboard) are applications, and the library crates on crates.io
are weeks old. The format is undocumented and moves with every Claude Code
release, so the parser keeps the raw record, treats unknown types and keys as
data for a drift report, and never fails a whole file on one bad line.

## License

MIT.

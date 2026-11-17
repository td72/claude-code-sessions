# CLAUDE.md

## Concept

`iktinos` turns the Claude Code transcripts on this machine
(`~/.claude/projects/*/<session id>.jsonl`, subagents under
`<session id>/subagents/`) and, optionally, live events from the
`iktinos-bridge` mod into a model of every session: `Session` /
`SessionStore`. It is a library first (`iktinos-core`);
[vig](https://github.com/td72/vig) renders it as a page (vig issue #222), and
the `iktinos` command is a terminal viewer over it.

Hard rules:

- **Read-only.** The crates read files and listen on a socket. They never
  write to Claude Code's directories and never send anything to Claude Code.
  The one thing read outside those directories is a repository's `.git` (the
  directory, or a worktree's file and its `commondir`), to tell which
  sessions run in the same repository (`place`); no `git` is started.
- **The crates do not log what they read, and do not hoard it.** The
  transcripts are the user's own files, so a view may model the text it needs
  (the last prompt, the first line of a prompt or of a response, a
  subagent's description, a tool call's command or path, the start of what a
  tool call returned). What a response thought is never modelled, only how
  long it took. But no
  crate writes record contents to a log by itself — `Record`'s `Debug` leaves
  the raw value out, and the `Debug` of a tool's output says how much there
  is, not what — and a `Session` keeps bounded recent activity, never the full
  history. The whole of a tool call's output is read again from the
  transcript on demand (`output::read_tool_output`) and kept nowhere; a store
  keeps even the excerpt only when it is asked to.
- **The format is undocumented and changes with every Claude Code release.**
  Keep the raw record, treat unknown `type`s and keys as data for the drift
  survey, skip records that lack a needed key, and never fail a whole file on
  one bad line.
- **Two inputs, one state.** Transcripts own existence, tokens, cost and
  history. Live events own freshness (running tool, waiting for input, context
  fill, rate limits). No field is written by both.
- **serve is local, read-only, and returns modelled shapes only.** It
  listens on `127.0.0.1` and has no option to listen elsewhere; it answers
  only its own `Host`, only `GET`, and only with the cookie its token was
  exchanged for. Its JSON is built from the model, never from a raw record.
  What reads a file again takes ids, never a path. Keep every one of these
  when adding to it.
- **No async runtime.** Threads and `mpsc`, like vig. No tokio, no hyper; the
  socket server parses its minimal HTTP/1.1 itself.
- **Fixtures are synthetic.** Nothing under `tests/fixtures/` may come from a
  real session. Record the shape, invent the content.

## Layout

```
crates/iktinos-core/   the library: record, discover, tail, session, follow, trace, output, place, pricing
crates/iktinos-live/   event protocol, Unix-socket server, merge (unix only)
crates/iktinos-tui/    the terminal viewer, as a library
crates/iktinos-web/    the page and the local server behind `iktinos serve`, as a library
crates/iktinos/        the `iktinos` command: the way in, and nothing else
mods/iktinos-bridge/               the Claude Code mod (TS) that speaks the protocol
tests/fixtures/<claude code version>/   synthetic transcripts and event samples
tape/, assets/                     demo tapes (recorded in a container) and their GIFs
```

Crates are versioned independently (core stays stable while live follows the
mod API). Publish order: core → live → tui → web → iktinos.

The crate named after the project is the command, so that
`cargo install iktinos` installs it. What the command does belongs in the
crate it starts (`iktinos-tui`, later others behind subcommands), never in
`crates/iktinos` itself.

## Build & Test

```bash
mise run web:build       # the page of `iktinos serve`, into web/dist (needs Node, from mise)
cargo build --workspace  # embeds web/dist if it is there, a placeholder if not
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

## Branch Workflow

Never commit directly to `main`. Pull the latest `main`, then branch:

```bash
git checkout main && git pull
git checkout -b feat/<name>      # or fix/<name>, chore/<name>
```

## Release

One crate per release pull request, from a branch named
`release/<crate>-<version>` (for example `release/iktinos-core-0.3.1`)
that sets the crate's `version`. Merging it runs `release.yml`: publish to
crates.io through Trusted Publishing, then the tag and GitHub release
`<crate>-v<version>`. Several crates: core → live → tui → web → iktinos, each in its own pull
request, each merged after the previous one is on crates.io.

A crate's first version cannot use Trusted Publishing (the crate must exist to
be configured): publish it by hand, then add the trusted publisher on
crates.io (repository `td72/iktinos`, workflow `release.yml`,
environment `release`).

## Conventions

- **Issues / PRs**: present the title and body in Japanese for review first;
  after approval translate to English and create them with `gh`. Label issues
  (`enhancement`, `bug`, `documentation`).
- **Commit messages**: gitmoji prefix — `✨` feature, `🐛` fix, `🩹` minor
  fix, `♻️` refactor, `🔧` config, `📝` docs, `✅` tests, `🔖` release.
- **GitHub Actions** are locked with `gh actions-lock`
  (`.github/workflows/actions.lock`, verified by `verify-lockfile.yml`). After
  changing any `uses:`, rerun `gh actions-lock` and commit the lockfile with it.
- **Demo GIFs**: `tape/<name>.tape` is recorded to `assets/<name>.gif` with
  `mise run demo:<name>` (`demo:all` for every one), which runs VHS in a
  container (`tape/record.sh`): only `tape/` and `assets/` are mounted, there
  is no network, and the sessions shown are the invented ones
  `tape/demo-fixture.py` writes there. Never run `vhs` on the host — iktinos
  would show the machine's real sessions — and never start `iktinos` in a
  tape without `--projects-dir` and `--socket-dir`. A change to the viewer's
  look or keys updates the tape and its gif in the same pull request.
  A gif recorded this way holds nothing real and needs no review of its own
  before it is pushed. One recorded any other way is never pushed: the
  repository is public, so a push publishes it.
- **Docs**: the README describes the design; each crate's `lib.rs` carries the
  module map. Keep them in step with the code.

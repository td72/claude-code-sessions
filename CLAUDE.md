# CLAUDE.md

## Concept

`claude-code-sessions` turns the Claude Code transcripts on this machine
(`~/.claude/projects/*/<session id>.jsonl`, subagents under
`<session id>/subagents/`) and, optionally, live events from the
`sessions-bridge` mod into a model of every session: `Session` /
`SessionStore`. It is a library first; [vig](https://github.com/td72/vig)
renders it as a page (vig issue #222), and `claude-code-sessions-tui` is a
small standalone viewer.

Hard rules:

- **Read-only.** The crates read files and listen on a socket. They never
  write to Claude Code's directories and never send anything to Claude Code.
- **The crates do not log what they read, and do not hoard it.** The
  transcripts are the user's own files, so a view may model the text it needs
  (the last prompt, a subagent's description, later a tool call's command or
  path). But no crate writes record contents to a log by itself — `Record`'s
  `Debug` leaves the raw value out — and a `Session` keeps bounded recent
  activity, never the full history.
- **The format is undocumented and changes with every Claude Code release.**
  Keep the raw record, treat unknown `type`s and keys as data for the drift
  survey, skip records that lack a needed key, and never fail a whole file on
  one bad line.
- **Two inputs, one state.** Transcripts own existence, tokens, cost and
  history. Live events own freshness (running tool, waiting for input, context
  fill, rate limits). No field is written by both.
- **No async runtime.** Threads and `mpsc`, like vig. No tokio, no hyper; the
  socket server parses its minimal HTTP/1.1 itself.
- **Fixtures are synthetic.** Nothing under `tests/fixtures/` may come from a
  real session. Record the shape, invent the content.

## Layout

```
crates/claude-code-sessions/        core: record, discover, tail, session, pricing
crates/claude-code-sessions-live/   event protocol, Unix-socket server, merge (unix only)
crates/claude-code-sessions-tui/    the `claude-code-sessions` binary
mods/sessions-bridge/               the Claude Code mod (TS) that speaks the protocol
tests/fixtures/<claude code version>/   synthetic transcripts and event samples
```

Crates are versioned independently (core stays stable while live follows the
mod API). Publish order: core → live → tui.

## Build & Test

```bash
cargo build --workspace
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
`release/<crate>-<version>` (for example `release/claude-code-sessions-0.1.1`)
that sets the crate's `version`. Merging it runs `release.yml`: publish to
crates.io through Trusted Publishing, then the tag and GitHub release
`<crate>-v<version>`. Several crates: core → live → tui, each in its own pull
request, each merged after the previous one is on crates.io.

A crate's first version cannot use Trusted Publishing (the crate must exist to
be configured): publish it by hand, then add the trusted publisher on
crates.io (repository `td72/claude-code-sessions`, workflow `release.yml`,
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
- **Docs**: the README describes the design; each crate's `lib.rs` carries the
  module map. Keep them in step with the code.

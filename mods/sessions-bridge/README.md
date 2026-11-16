# sessions-bridge

A Claude Code mod that tells every running consumer of `claude-code-sessions`
(vig, the standalone viewer) what a session is doing right now: the turn in
progress, the tool running, a permission prompt waiting for an answer, the
context against the window in effect, the rate limits.

Without the mod the consumers still work from the transcripts alone; the mod
only adds freshness.

## Install

At the prompt of a Claude Code session:

```
/plugin install sessions-bridge --marketplace td72/claude-code-sessions
```

From a shell:

```
claude plugin install sessions-bridge --marketplace td72/claude-code-sessions
```

## What it does

Each consumer listens on its own Unix socket under
`~/.local/state/claude-code-sessions/sockets/`. On every event below the mod
lists that directory and posts one small JSON object to each socket
(`POST http://claude-code-sessions/events` over the socket). It never waits
for a consumer, and a socket that refuses is ignored. With no consumer running
it sends nothing.

| event | sent when | carries |
| --- | --- | --- |
| `session.start` | the session starts, resumes, or the mod reloads | cwd, model |
| `session.end` | the process exits | the reason |
| `turn.start`, `turn.complete` | a turn begins and ends | turn id, how it ended, its duration |
| `tool.check` | a tool call is checked | tool name, the decision (`allow`, `ask`, `deny`) |
| `tool.call` | a tool call starts and ends | tool name, a one-line summary (the command, the file path), duration, success |
| `agent.spawn` | a subagent starts | its type, description and model |
| `session.measure` | Claude Code's measurements change | context tokens and window, rate limits, cost |

The mod only observes: every hook hands the event on unchanged. It sends
names, ids, numbers and the one-line summary of a tool call. It never sends a
prompt, a response, a tool's output or the environment, and nothing leaves the
machine.

The exact shapes are the samples under
[`tests/fixtures/events/`](../../tests/fixtures/events) (protocol version 1,
defined by the `claude-code-sessions-live` crate).

## Developing

```bash
claude plugin validate mods/sessions-bridge
claude plugin test mods/sessions-bridge
claude --plugin-dir mods/sessions-bridge      # a session with the mod loaded from disk
```

`hooks/samples.ts` is generated from the samples; after changing one, run
`UPDATE_SAMPLES=1 cargo test -p claude-code-sessions-live --test mod_samples`.
The mod's tests then check that what it sends still matches.

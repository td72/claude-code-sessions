# claude-code-sessions-bridge

A Claude Code mod that sends live session events to every running consumer of
`claude-code-sessions` (vig, the standalone viewer).

Not implemented yet. The protocol it will speak is defined by the
`claude-code-sessions-live` crate (`event` module) and lives in this repository
so both ends change together.

- On every send the mod lists `~/.local/state/claude-code-sessions/sockets/`
  and POSTs the event to each socket with
  `$.http.fetch("http://claude-code-sessions/events", { method: "POST", socketPath })`.
  Sockets that refuse the connection are ignored.
- Events: `session.start`, `turn.start` / `turn.complete`, `tool.call` start and
  end (tool name only), `tool.check`, `session.measure`, `agent.spawn`,
  `session.end`. Events go to local Unix sockets only and stay small: names, ids, numbers and short summaries.
- Without the mod the transcripts alone drive the model; the mod only adds
  freshness.

Install (once published): `/plugin install claude-code-sessions-bridge --marketplace td72/claude-code-sessions`

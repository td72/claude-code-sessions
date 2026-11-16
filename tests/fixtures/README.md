# Fixtures

Everything here is synthetic. The shape of each record (its keys, their types,
the order records appear in) was recorded from the Claude Code version the
directory is named after; every value was invented. Nothing may be copied from
a real session.

Text the views do not model (responses, tool outputs, the prompts inside
`user` records, and the fields of a tool input that are not summarised)
contains the marker `SYNTHETIC-TEXT`. The tests assert
that the marker never appears in a view, a `Debug` output or a report, so a new
fixture must keep using it. Text a view does model (a title, the last prompt,
the command or path a tool call is summarised by) is plain invented text
without the marker.

| file | what it holds |
| --- | --- |
| `plain-turn.jsonl` | a prompt and a two-block response (one `message.id`, two records), turn duration, title, last prompt |
| `tool-call.jsonl` | three tool calls: a success, an error, a result without `is_error` |
| `subagent.jsonl`, `subagent.meta.json` | a subagent transcript and its metadata |
| `malformed.jsonl` | garbage, a truncated line and a bare scalar between two good records |
| `unknown.jsonl` | a record type, keys, a content block, an attachment type and a system subtype the crate does not know, for the drift survey |
| `unknown-model.jsonl` | the plain turn answered by a model the price table does not know |
| `resumed.jsonl` | a session that exited (`cost-state`) and was resumed for one more turn |
| `projects/` | a projects directory: an indexed and an unindexed session in one project, a session with two subagents (one without metadata) in another |

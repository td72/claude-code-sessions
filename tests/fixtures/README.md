# Fixtures

Everything here is synthetic. The shape of each record (its keys, their types,
the order records appear in) was recorded from the Claude Code version the
directory is named after; every value was invented. Nothing may be copied from
a real session.

Text the views do not model (a response after its first line, what it
thought, what a tool returned past what a view keeps, a prompt after its first line, a subagent's instructions, and the
fields of a tool input that are not summarised) contains the marker `SYNTHETIC-TEXT`. The tests assert
that the marker never appears in a view, a `Debug` output or a report, so a new
fixture must keep using it. Text a view does model (a title, the last prompt,
the first line of a prompt or of a response, the command or path a tool call
is summarised by,
the start of what a tool call returned) is plain invented text without the
marker.

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
| `trace/` | a projects directory with one session of three interactions: a tool call; a subagent spawned by a tool call (its metadata names the call); one cut off while a tool runs |
| `responses/` | a projects directory with one session of one interaction: a response that thinks, speaks and calls two tools at once; one that thinks and calls a tool and says nothing; one whose `api-request` has no `base` and another `shapeHash`, stopped by `max_tokens` |
| `output/` | a projects directory with one session whose calls return every kind of output: a success, an error, one with stderr, one over what a view keeps (the marker is past the limit), one with a block that is not text, one moved to `tool-results/`, one interrupted |

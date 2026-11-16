# Event samples

One file per event of the live protocol (`iktinos-live`, `event`
module), version 1. They are the contract between the bridge mod and the
crate: the crate's tests parse and re-serialize every file, and the mod's
tests check that what it sends has these shapes.

All values are invented. The events describe the session of
`../2.1.325/tool-call.jsonl` and its subagent.

`unknown-type.json` is a type the protocol does not define: a consumer must
accept it and ignore it.

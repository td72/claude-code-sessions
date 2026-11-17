#!/usr/bin/env python3
"""Write synthetic Claude Code sessions for the demo tapes.

    demo-fixture.py <projects-dir> <socket-dir> [live]

Everything here is invented: the record shapes follow Claude Code's
transcripts and the live protocol of `iktinos-live`, no value comes from a
real session. Timestamps count back from now.

Four sessions in three repositories are written at once. With `live`, one
of them then keeps working for about a minute: its transcript grows call
by call, and the events the bridge mod would send are posted to every
socket in <socket-dir>. Another waits for a permission, which only the
events can say; a while later it is allowed, and runs its command.
"""

import json
import socket
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

VERSION = "2.1.325"
MAIN_MODEL = "claude-fable-5-2"
AGENT_MODEL = "claude-haiku-5-5"
WINDOW = 200_000

# The invented repositories. Nothing has to exist at these paths.
ATLAS = "/work/atlas"
HARBOR = "/work/harbor-web"
NOTES = "/work/field-notes"


def stamp(at: datetime) -> str:
    return at.strftime("%Y-%m-%dT%H:%M:%S.") + f"{at.microsecond // 1000:03d}Z"


def now() -> datetime:
    return datetime.now(timezone.utc)


class Session:
    """One session's transcript, written record by record at a moving clock."""

    def __init__(self, projects: Path, session_id: str, cwd: str, branch: str, ago: timedelta):
        self.id = session_id
        self.cwd = cwd
        self.branch = branch
        project = projects / cwd.replace("/", "-").replace(".", "-")
        self.main = project / f"{session_id}.jsonl"
        self.agents = project / session_id / "subagents"
        self.agents.mkdir(parents=True, exist_ok=True)
        self.clock = now() - ago
        self.n = 0
        self.prompt_id = ""
        self.context = 18_000
        self.started = self.clock
        # While the recording runs, waiting takes real time.
        self.realtime = False
        self.record({"type": "mode", "mode": "normal"}, common=False)

    # --- the clock ---

    def wait(self, seconds: float):
        if self.realtime:
            time.sleep(seconds)
            self.clock = now()
        else:
            self.clock += timedelta(seconds=seconds)

    def uid(self, prefix: str) -> str:
        self.n += 1
        return f"{prefix}-{self.id[:4]}-{self.n:04d}"

    # --- records ---

    def record(self, record: dict, agent: str | None = None, common: bool = True):
        record["sessionId"] = self.id
        if common:
            record.setdefault("timestamp", stamp(self.clock))
            record.setdefault("userType", "external")
            record.setdefault("entrypoint", "cli")
            record.setdefault("cwd", self.cwd)
            record.setdefault("version", VERSION)
            record.setdefault("gitBranch", self.branch)
        path = self.main
        if agent:
            record["agentId"] = agent
            path = self.agents / f"agent-{agent}.jsonl"
        with path.open("a") as out:
            out.write(json.dumps(record) + "\n")

    def title(self, text: str):
        self.record({"type": "ai-title", "aiTitle": text}, common=False)

    def prompt(self, text: str, agent: str | None = None):
        """What was asked: the start of an interaction (or of a subagent)."""
        self.prompt_id = self.uid("prompt")
        self.record(
            {
                "parentUuid": None,
                "isSidechain": agent is not None,
                "promptId": self.prompt_id,
                "type": "user",
                "message": {"role": "user", "content": text},
                "uuid": self.uid("u"),
                "permissionMode": "default",
                "origin": {"kind": "human"},
                "promptSource": "typed",
                "turnOrigin": "user",
                "turnPosition": 0,
            },
            agent,
        )
        if agent is None:
            self.record({"type": "last-prompt", "lastPrompt": text}, common=False)

    def request(self, agent: str | None = None, kind: str = "Explore"):
        """The request to the model that the next response answers."""
        self.ref = self.uid("ref")
        self.record(
            {
                "type": "api-request",
                "id": self.ref,
                "timestamp": stamp(self.clock),
                "version": VERSION,
                "querySource": f"agent:builtin:{kind}" if agent else "repl_main_thread",
                "shapeHash": "shape-0001",
                "requestDigest": "digest-0001",
                "base": "base-0001",
                "params": {},
                "keep": [],
                "tail": [],
            },
            agent,
            common=False,
        )

    def response(self, content: list, stop: str | None, agent: str | None = None,
                 msg: str | None = None):
        """One block of a response. Its blocks share `msg`, each in a record."""
        if agent is None and stop is not None:
            self.context += 2_400
        read = 21_000 if agent else self.context
        self.record(
            {
                "parentUuid": self.uid("p"),
                "isSidechain": agent is not None,
                "type": "assistant",
                "uuid": self.uid("a"),
                "requestRef": self.ref,
                "message": {
                    "model": AGENT_MODEL if agent else MAIN_MODEL,
                    "id": msg or self.uid("msg"),
                    "type": "message",
                    "role": "assistant",
                    "content": content,
                    "stop_reason": stop,
                    "stop_sequence": None,
                    "usage": {
                        "input_tokens": 24,
                        "cache_creation_input_tokens": 2_400,
                        "cache_read_input_tokens": read,
                        "cache_creation": {
                            "ephemeral_5m_input_tokens": 0,
                            "ephemeral_1h_input_tokens": 2_400,
                        },
                        "output_tokens": 180 if stop == "end_turn" else 420,
                        "service_tier": "standard",
                        "speed": "standard",
                    },
                },
            },
            agent,
        )

    def use(self, name: str, tool_input: dict, think: float = 1.5, agent: str | None = None,
            say: str | None = None, thought: bool = False) -> str:
        """The model asks for a tool call, `think` seconds after it was asked:
        after thinking (`thought`), and after saying `say`, if it does."""
        self.request(agent)
        msg = self.uid("msg")
        if thought:
            self.wait(think * 0.7)
            self.response([{"type": "thinking", "thinking": "SYNTHETIC", "signature": "sig"}],
                          None, agent, msg)
            self.wait(think * 0.3)
        else:
            self.wait(think)
        if say:
            self.response([{"type": "text", "text": say}], None, agent, msg)
        tool_id = self.uid("toolu")
        self.response(
            [
                {
                    "type": "tool_use",
                    "id": tool_id,
                    "name": name,
                    "input": tool_input,
                    "caller": {"type": "direct"},
                }
            ],
            "tool_use",
            agent,
            msg,
        )
        return tool_id

    def result(self, tool_id: str, output: str, ok: bool = True, bash: bool = False,
               agent: str | None = None):
        """What the tool call returned."""
        record = {
            "parentUuid": self.uid("p"),
            "isSidechain": agent is not None,
            "promptId": self.prompt_id,
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {
                        "tool_use_id": tool_id,
                        "type": "tool_result",
                        "content": output,
                        "is_error": not ok,
                    }
                ],
            },
            "uuid": self.uid("r"),
            "permissionDecision": {"behavior": "allow"},
        }
        if bash and ok:
            record["toolUseResult"] = {
                "stdout": output,
                "stderr": "",
                "interrupted": False,
                "isImage": False,
                "noOutputExpected": False,
            }
        elif not ok:
            record["toolUseResult"] = output
        self.record(record, agent)

    def call(self, name: str, tool_input: dict, output: str, took: float = 0.4,
             think: float = 1.5, ok: bool = True, agent: str | None = None,
             say: str | None = None, thought: bool = False) -> str:
        """A whole tool call: asked for, run for `took` seconds, returned."""
        tool_id = self.use(name, tool_input, think, agent, say, thought)
        self.wait(took)
        self.result(tool_id, output, ok, name == "Bash", agent)
        self.wait(0.1)
        return tool_id

    def answer(self, text: str, think: float = 2.5, agent: str | None = None,
               began: datetime | None = None):
        """The last response of an interaction, and how long the turn took."""
        self.request(agent)
        self.wait(think)
        self.response([{"type": "text", "text": text}], "end_turn", agent)
        if agent is None and began is not None:
            self.wait(0.1)
            took = int((self.clock - began).total_seconds() * 1000)
            self.record(
                {
                    "parentUuid": self.uid("p"),
                    "isSidechain": False,
                    "type": "system",
                    "subtype": "turn_duration",
                    "durationMs": took,
                    "messageCount": 8,
                    "uuid": self.uid("s"),
                    "isMeta": False,
                }
            )

    def meta(self, agent: str, kind: str, task: str, spawned_by: str, done: bool = True):
        meta = {
            "agentType": kind,
            "isBuiltIn": True,
            "description": task,
            "toolUseId": spawned_by,
            "spawnDepth": 1,
        }
        if done:
            meta["finished"] = True
        (self.agents / f"agent-{agent}.meta.json").write_text(json.dumps(meta))


# --- invented tool output ---

TESTS_FAILED = """running 14 tests
test backoff::tests::doubles_each_attempt ... ok
test backoff::tests::stops_at_the_cap ... FAILED
test fetch::tests::retries_a_timeout ... ok

failures:
    backoff::tests::stops_at_the_cap

test result: FAILED. 13 passed; 1 failed; 0 ignored"""

TESTS_OK = """running 14 tests
test backoff::tests::doubles_each_attempt ... ok
test backoff::tests::stops_at_the_cap ... ok
test fetch::tests::retries_a_timeout ... ok

test result: ok. 14 passed; 0 failed; 0 ignored"""

CLIPPY_OK = """    Checking atlas v0.4.0 (/work/atlas)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.12s"""

SOURCE = "pub fn fetch(url: &str) -> Result<Body, Error> {\n    // invented for the demo\n}"


def atlas(projects: Path) -> Session:
    """The session that is still working when the recording starts."""
    s = Session(projects, "a71a5000-0000-4000-8000-00000000d3e0", ATLAS,
                "feat/retry-backoff", timedelta(minutes=14))
    s.title("Add retry with backoff to the fetcher")

    # Look around, with a subagent.
    began = s.clock
    s.prompt("Where does fetch() get called, and is there any retry today?")
    s.call("Read", {"file_path": f"{ATLAS}/src/fetch.rs"}, SOURCE, 0.3,
           say="I'll start with the fetcher itself.")
    s.call("Read", {"file_path": f"{ATLAS}/Cargo.toml"}, '[package]\nname = "atlas"', 0.2)
    explore = s.use("Agent", {"description": "Find every caller of fetch()",
                              "prompt": "SYNTHETIC", "subagent_type": "Explore"},
                    3.0, thought=True)
    agent = "a1b2c3d4e5f60718"
    s.meta(agent, "Explore", "Find every caller of fetch()", explore)
    s.wait(0.5)
    s.prompt("SYNTHETIC subagent prompt", agent)
    s.call("Grep", {"pattern": "fetch\\(", "path": ATLAS},
           "src/main.rs:41\nsrc/sync.rs:17\nsrc/sync.rs:88", 0.4, agent=agent)
    s.call("Read", {"file_path": f"{ATLAS}/src/main.rs"}, SOURCE, 0.2, agent=agent)
    s.call("Read", {"file_path": f"{ATLAS}/src/sync.rs"}, SOURCE, 0.3, agent=agent)
    s.call("Glob", {"pattern": "tests/**/*.rs"}, "tests/fetch.rs\ntests/sync.rs", 0.2, agent=agent)
    s.answer("Three callers, none of them retries.", 3.0, agent)
    s.wait(0.3)
    s.result(explore, "Three callers: main.rs:41, sync.rs:17, sync.rs:88. No retry anywhere.")
    s.answer("fetch() is called from three places, and nothing retries today.", 4.0, began=began)

    # The change itself, with one failing test run.
    s.wait(95)
    began = s.clock
    s.prompt("Add retry with exponential backoff to the fetcher")
    s.call("Edit", {"file_path": f"{ATLAS}/src/fetch.rs"}, "The file has been updated.", 0.4, 6.0,
           thought=True)
    s.call("Write", {"file_path": f"{ATLAS}/src/backoff.rs"}, "File created.", 0.3, 9.0)
    s.call("Edit", {"file_path": f"{ATLAS}/src/main.rs"}, "The file has been updated.", 0.3, 3.0)
    s.call("Bash", {"command": "cargo test --workspace"}, TESTS_FAILED, 38.4, 2.0, ok=False,
           say="Now the tests.")
    s.call("Edit", {"file_path": f"{ATLAS}/src/backoff.rs"}, "The file has been updated.", 0.4, 7.0,
           say="The cap is applied before the doubling. Fixing that.", thought=True)
    s.call("Bash", {"command": "cargo test --workspace"}, TESTS_OK, 41.7, 2.0)
    s.answer("The fetcher now retries with exponential backoff, capped at 30s.", 3.5, began=began)

    # Polish: still going when the recording starts.
    s.clock = now() - timedelta(seconds=50)
    s.live_began = s.clock
    s.prompt("Document the backoff and run clippy")
    s.call("Edit", {"file_path": f"{ATLAS}/README.md"}, "The file has been updated.", 0.3, 4.0)
    s.call("Bash", {"command": "cargo clippy --all-targets -- -D warnings"}, CLIPPY_OK, 12.6, 2.0)
    return s


def atlas_deps(projects: Path) -> Session:
    """A second session in the same repository: it will wait for a permission."""
    s = Session(projects, "de9500d5-0000-4000-8000-00000000d3e0", ATLAS,
                "chore/bump-deps", timedelta(minutes=6))
    s.title("Bump serde and tokio to their latest minor")
    s.prompt("Bump serde and tokio to their latest minor versions")
    s.call("Read", {"file_path": f"{ATLAS}/Cargo.toml"}, '[package]\nname = "atlas"', 0.2)
    s.call("Bash", {"command": "cargo update --dry-run"},
           "    Updating serde v1.0.210 -> v1.0.219\n    Updating tokio v1.40.0 -> v1.44.1", 3.1)
    s.call("Edit", {"file_path": f"{ATLAS}/Cargo.toml"}, "The file has been updated.", 0.3, 2.0)
    # Asked for, not yet allowed: `cargo update` waits for the user.
    s.pending = s.use("Bash", {"command": "cargo update"}, 2.0)
    return s


def harbor(projects: Path) -> Session:
    """Finished a while ago: two interactions, one with a subagent."""
    s = Session(projects, "4a2b0000-0000-4000-8000-00000000d3e0", HARBOR,
                "fix/date-picker-flake", timedelta(minutes=52))
    s.title("Fix the flaky date picker test")

    began = s.clock
    s.prompt("The date picker test fails about one run in five. Find out why.")
    s.call("Bash", {"command": "pnpm vitest run src/date-picker"},
           " FAIL  src/date-picker/picker.test.tsx > closes on outside click\n"
           " Test Files  1 failed (1)", 9.8, ok=False)
    s.call("Read", {"file_path": f"{HARBOR}/src/date-picker/picker.test.tsx"},
           "it('closes on outside click', async () => { /* invented */ })", 0.3)
    task = s.use("Agent", {"description": "Look for timers the test does not await",
                           "prompt": "SYNTHETIC", "subagent_type": "Explore"})
    agent = "c3d4e5f607182930"
    s.meta(agent, "Explore", "Look for timers the test does not await", task)
    s.wait(0.5)
    s.prompt("SYNTHETIC subagent prompt", agent)
    s.call("Grep", {"pattern": "setTimeout|requestAnimationFrame", "path": f"{HARBOR}/src"},
           "src/date-picker/popover.tsx:58\nsrc/hooks/use-outside-click.ts:22", 0.5, agent=agent)
    s.call("Read", {"file_path": f"{HARBOR}/src/hooks/use-outside-click.ts"},
           "export function useOutsideClick() { /* invented */ }", 0.2, agent=agent)
    s.answer("The listener is registered in a timeout the test does not wait for.", 2.5, agent)
    s.wait(0.3)
    s.result(task, "use-outside-click.ts:22 registers its listener in a timeout.")
    s.answer("The test clicks outside before the listener exists.", 5.0, began=began)

    s.wait(140)
    began = s.clock
    s.prompt("Make the test wait for the listener instead of sleeping")
    s.call("Edit", {"file_path": f"{HARBOR}/src/date-picker/picker.test.tsx"},
           "The file has been updated.", 0.3, 5.0)
    s.call("Bash", {"command": "pnpm vitest run src/date-picker --repeat 20"},
           " Test Files  1 passed (1)\n      Tests  120 passed (120)", 31.2)
    s.answer("Twenty runs in a row pass now.", 3.0, began=began)
    return s


def notes(projects: Path) -> Session:
    """An older, short session."""
    s = Session(projects, "0e5e0000-0000-4000-8000-00000000d3e0", NOTES, "main",
                timedelta(hours=3, minutes=10))
    s.title("Turn the workshop notes into an outline")
    began = s.clock
    s.prompt("Turn notes/workshop.md into an outline with one heading per session")
    s.call("Read", {"file_path": f"{NOTES}/notes/workshop.md"}, "# Workshop\n(invented)", 0.2)
    s.call("Write", {"file_path": f"{NOTES}/notes/outline.md"}, "File created.", 0.3, 11.0,
           thought=True)
    s.answer("The outline is in notes/outline.md, one heading per session.", 2.0, began=began)
    return s


# --- the live side ---


class Bridge:
    """Posts events the way the bridge mod does: to every socket there is."""

    def __init__(self, sockets: Path):
        self.sockets = sockets
        self.known: set[Path] = set()
        # Events a consumer that starts later still has to hear.
        self.standing: list[dict] = []

    def post(self, path: Path, event: dict):
        body = json.dumps({"v": 1, "ts": int(time.time() * 1000), **event}).encode()
        head = f"POST /events HTTP/1.1\r\nContent-Length: {len(body)}\r\n\r\n".encode()
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as out:
                out.settimeout(1)
                out.connect(str(path))
                out.sendall(head + body)
                out.recv(256)
        except OSError:
            self.known.discard(path)

    def send(self, event: dict, standing: bool = False):
        if standing:
            self.standing.append(event)
        found = set(self.sockets.glob("*.sock")) if self.sockets.is_dir() else set()
        for path in sorted(found - self.known):
            self.known.add(path)
            for earlier in self.standing:
                if earlier is not event:
                    self.post(path, earlier)
        for path in sorted(found):
            self.post(path, event)


def work(s: Session, waiting: Session, bridge: Bridge):
    """The session keeps working, in real time, for about a minute."""
    turn = {"type": "turn.start", "session_id": s.id, "turn_id": "turn-3"}
    bridge.send({"type": "session.start", "session_id": s.id, "cwd": s.cwd,
                 "model": MAIN_MODEL, "interactive": True}, standing=True)
    bridge.send(turn, standing=True)
    bridge.send({"type": "session.start", "session_id": waiting.id, "cwd": waiting.cwd,
                 "model": MAIN_MODEL, "interactive": True}, standing=True)
    bridge.send({"type": "turn.start", "session_id": waiting.id, "turn_id": "turn-1"},
                standing=True)
    bridge.send({"type": "tool.check", "session_id": waiting.id, "tool": "Bash",
                 "tool_use_id": waiting.pending, "decision": "ask"}, standing=True)

    ask = bridge.standing[-1]
    steps = [
        ("Edit", {"file_path": f"{ATLAS}/src/backoff.rs"}, "The file has been updated.", 0.5, 3.0,
         "Documenting the cap first."),
        ("Bash", {"command": "cargo fmt --all --check"}, "", 2.5, 2.0, None),
        ("Read", {"file_path": f"{ATLAS}/tests/fetch.rs"}, SOURCE, 0.3, 2.5, None),
        ("Edit", {"file_path": f"{ATLAS}/tests/fetch.rs"}, "The file has been updated.", 0.4, 4.0,
         "A test for the cap as well."),
        ("Bash", {"command": "cargo test backoff"}, TESTS_OK, 7.0, 2.0, None),
        ("Edit", {"file_path": f"{ATLAS}/README.md"}, "The file has been updated.", 0.4, 3.5, None),
        ("Bash", {"command": "cargo doc --no-deps"}, " Documenting atlas v0.4.0", 6.0, 2.0, None),
        ("Bash", {"command": "cargo clippy --all-targets -- -D warnings"}, CLIPPY_OK, 8.0, 2.5,
         "Clippy once more."),
        ("Read", {"file_path": f"{ATLAS}/CHANGELOG.md"}, "# Changelog\n(invented)", 0.3, 2.0, None),
        ("Edit", {"file_path": f"{ATLAS}/CHANGELOG.md"}, "The file has been updated.", 0.4, 4.0,
         None),
        ("Bash", {"command": "cargo test --workspace"}, TESTS_OK, 9.0, 2.0, None),
    ]
    used = 31.0
    s.realtime = True
    s.clock = now()
    for index, (name, tool_input, output, took, think, say) in enumerate(steps):
        tool_id = s.use(name, tool_input, think, say=say)
        summary = tool_input.get("command") or tool_input.get("file_path")
        bridge.send({"type": "tool.check", "session_id": s.id, "tool": name,
                     "tool_use_id": tool_id, "decision": "allow"})
        bridge.send({"type": "tool.call", "session_id": s.id, "phase": "start", "tool": name,
                     "tool_use_id": tool_id, "summary": summary})
        s.wait(took)
        s.result(tool_id, output, bash=name == "Bash")
        bridge.send({"type": "tool.call", "session_id": s.id, "phase": "end", "tool": name,
                     "tool_use_id": tool_id, "duration_ms": int(took * 1000), "success": True})
        used += 0.6
        bridge.send({"type": "session.measure", "session_id": s.id,
                     "context": {"tokens": s.context, "window": WINDOW},
                     "rate_limits": [{"kind": "five_hour", "percent_used": used},
                                     {"kind": "seven_day", "percent_used": 12.0}]})
        # Meanwhile the other session is allowed its command, and runs it.
        if index == 3:
            bridge.standing.remove(ask)
            bridge.send({"type": "tool.check", "session_id": waiting.id, "tool": "Bash",
                         "tool_use_id": waiting.pending, "decision": "allow"})
            bridge.send({"type": "tool.call", "session_id": waiting.id, "phase": "start",
                         "tool": "Bash", "tool_use_id": waiting.pending,
                         "summary": "cargo update"})
        if index == 5:
            waiting.clock = now()
            waiting.result(waiting.pending, "    Updating serde v1.0.210 -> v1.0.219\n"
                           "    Updating tokio v1.40.0 -> v1.44.1", bash=True)
            bridge.send({"type": "tool.call", "session_id": waiting.id, "phase": "end",
                         "tool": "Bash", "tool_use_id": waiting.pending,
                         "duration_ms": 9000, "success": True})

    s.answer("Documented, and clippy is clean.", 3.0, began=s.live_began)
    bridge.send({"type": "turn.complete", "session_id": s.id, "turn_id": "turn-3",
                 "reason": "answer"})


def main():
    projects, sockets = Path(sys.argv[1]), Path(sys.argv[2])
    live = len(sys.argv) > 3 and sys.argv[3] == "live"
    notes(projects)
    harbor(projects)
    waiting = atlas_deps(projects)
    working = atlas(projects)
    if live:
        work(working, waiting, Bridge(sockets))


if __name__ == "__main__":
    main()

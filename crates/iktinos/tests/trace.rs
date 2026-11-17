//! The trace: a session's latest interactions as trees of spans.

use std::fs;
use std::path::{Path, PathBuf};

use iktinos::discover::{discover, SessionFiles};
use iktinos::record::{Record, Timestamp, KNOWN_CLAUDE_CODE_VERSION};
use iktinos::session::{Session, SessionStore};
use iktinos::trace::{Item, Node, SpanKind, MAX_SPANS};

const ID: &str = "44444444-4444-4444-8444-444444444444";
const AGENT: &str = "c3d4e5f607182930";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
}

fn read(path: &Path) -> Vec<Record> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(Record::parse)
        .collect()
}

fn files() -> SessionFiles {
    let found = discover(&fixture_dir().join("trace")).unwrap();
    found.into_iter().find(|files| files.id == ID).unwrap()
}

/// The fixture session, its subagent's transcript read before or after the
/// main one.
fn folded(store: SessionStore, subagent_first: bool) -> Session {
    let files = files();
    let mut store = store;
    store.discovered(&files);
    let main = read(&files.transcript.path);
    let subagent = read(&files.subagents[0].transcript.path);
    if subagent_first {
        store.apply_records(ID, &subagent);
        store.apply_records(ID, &main);
    } else {
        store.apply_records(ID, &main);
        store.apply_records(ID, &subagent);
    }
    store.get(ID).unwrap().clone()
}

fn at(time: &str) -> Option<Timestamp> {
    Timestamp::parse(&format!("2026-11-04T{time}Z"))
}

/// A node as text: its depth, what it is and how long it took.
fn line(node: &Node) -> String {
    let what = match node.item {
        Item::Span(span) => match &span.kind {
            SpanKind::LlmRequest(request) => format!(
                "llm_request {} {}",
                request.query_source.as_deref().unwrap_or("?"),
                request.stop_reason.as_deref().unwrap_or("?")
            ),
            SpanKind::Tool(call) => format!("tool {} {:?}", call.name, call.success),
            _ => "?".to_owned(),
        },
        Item::Subagent(subagent) => format!("subagent {}", subagent.agent_id),
    };
    format!("{}{what} {:?}", "  ".repeat(node.depth), node.duration_ms())
}

fn tree(session: &Session, interaction: usize) -> Vec<String> {
    session
        .walk(&session.trace[interaction])
        .iter()
        .map(line)
        .collect()
}

#[test]
fn turns_are_kept_with_their_own_spans() {
    let session = folded(SessionStore::new(), false);
    assert_eq!(session.turns, 2, "two interactions were completed");
    let ids: Vec<&str> = session
        .trace
        .iter()
        .map(|interaction| interaction.id.as_str())
        .collect();
    assert_eq!(ids, ["prompt-t1", "prompt-t2", "prompt-t3"]);

    let first = &session.trace[0];
    // What was asked: the first line of the prompt, and no more of it.
    assert_eq!(
        first.prompt.as_deref(),
        Some("Synthetic prompt: run the checks")
    );
    assert_eq!(first.started_at, at("12:00:00.000"));
    assert_eq!(first.ended_at, at("12:00:05.100"));
    assert_eq!(first.duration_ms, Some(5000));
    // An LLM request runs from its `api-request` record to its last block,
    // a tool call from its block to its result; they are siblings.
    assert_eq!(
        tree(&session, 0),
        [
            "llm_request repl_main_thread tool_use Some(1900)",
            "tool Bash Some(true) Some(1000)",
            "llm_request repl_main_thread end_turn Some(1900)",
        ]
    );
    let SpanKind::LlmRequest(request) = &first.thread.spans[0].kind else {
        panic!()
    };
    assert_eq!(request.model.as_deref(), Some("claude-fable-5-2"));
    assert!(request.tokens.is_some_and(|tokens| tokens.output > 0));
    assert!(request.usd.is_some_and(|usd| usd > 0.0));
    let SpanKind::Tool(call) = &first.thread.spans[1].kind else {
        panic!()
    };
    assert_eq!(call.summary.as_deref(), Some("cargo fmt --all"));
}

#[test]
fn a_subagent_hangs_under_the_call_that_spawned_it() {
    let session = folded(SessionStore::new(), false);
    assert_eq!(
        tree(&session, 1),
        [
            "llm_request repl_main_thread tool_use Some(900)",
            "tool Agent Some(true) Some(6000)",
            format!("  subagent {AGENT} Some(4500)").as_str(),
            "    llm_request agent:builtin:Explore tool_use Some(1400)",
            "    tool Grep Some(true) Some(500)",
            "    llm_request agent:builtin:Explore end_turn Some(2400)",
            "llm_request repl_main_thread end_turn Some(1900)",
        ]
    );
    // It is in that interaction only.
    assert!(!tree(&session, 0)
        .iter()
        .any(|line| line.contains("subagent")));
    assert!(!tree(&session, 2)
        .iter()
        .any(|line| line.contains("subagent")));

    // Whichever transcript is read first, the trees are the same.
    let reversed = folded(SessionStore::new(), true);
    assert_eq!(session.trace, reversed.trace);
    assert_eq!(session.subagents, reversed.subagents);
    for interaction in 0..3 {
        assert_eq!(tree(&session, interaction), tree(&reversed, interaction));
    }
}

#[test]
fn a_turn_that_was_cut_off_has_no_end() {
    let session = folded(SessionStore::new(), false);
    let last = &session.trace[2];
    assert_eq!(last.started_at, at("12:02:00.000"));
    assert_eq!((last.ended_at, last.duration_ms), (None, None));
    assert_eq!(
        tree(&session, 2),
        [
            "llm_request repl_main_thread tool_use Some(1900)",
            "tool Bash None None",
        ]
    );
}

#[test]
fn a_subagent_without_its_link_is_placed_by_time() {
    let files = files();
    let mut store = SessionStore::new();
    // No metadata: nothing says which call spawned it.
    store.apply_records(ID, &read(&files.transcript.path));
    store.apply_records(ID, &read(&files.subagents[0].transcript.path));
    let session = store.get(ID).unwrap();
    assert_eq!(session.subagents[AGENT].tool_use_id, None);
    assert_eq!(
        tree(session, 1),
        [
            "llm_request repl_main_thread tool_use Some(900)",
            "tool Agent Some(true) Some(6000)",
            "llm_request repl_main_thread end_turn Some(1900)",
            format!("subagent {AGENT} Some(4500)").as_str(),
            "  llm_request agent:builtin:Explore tool_use Some(1400)",
            "  tool Grep Some(true) Some(500)",
            "  llm_request agent:builtin:Explore end_turn Some(2400)",
        ]
    );
    assert!(tree(session, 0)
        .iter()
        .all(|line| !line.contains("subagent")));
    assert!(tree(session, 2)
        .iter()
        .all(|line| !line.contains("subagent")));
}

#[test]
fn reading_the_same_records_again_changes_nothing() {
    let files = files();
    let once = folded(SessionStore::new(), false);
    let mut store = SessionStore::new();
    store.discovered(&files);
    for _ in 0..2 {
        store.apply_records(ID, &read(&files.transcript.path));
        store.apply_records(ID, &read(&files.subagents[0].transcript.path));
    }
    assert_eq!(store.get(ID).unwrap(), &once);

    // A reset forgets the trace; reading again builds the same one.
    store.reset(ID);
    assert!(store.get(ID).unwrap().trace.is_empty());
    assert!(store.get(ID).unwrap().subagents[AGENT]
        .thread
        .spans
        .is_empty());
    store.apply_records(ID, &read(&files.transcript.path));
    store.apply_records(ID, &read(&files.subagents[0].transcript.path));
    assert_eq!(store.get(ID).unwrap(), &once);
}

#[test]
fn the_oldest_turns_are_dropped_past_the_limit() {
    for subagent_first in [false, true] {
        let session = folded(
            SessionStore::new().with_trace_interactions(1),
            subagent_first,
        );
        let ids: Vec<&str> = session
            .trace
            .iter()
            .map(|interaction| interaction.id.as_str())
            .collect();
        assert_eq!(ids, ["prompt-t3"]);
        // The subagent of a dropped interaction keeps its totals, not its spans.
        let subagent = &session.subagents[AGENT];
        assert!(subagent.thread.spans.is_empty(), "{subagent_first}");
        assert_eq!(subagent.usage.responses, 2);
        assert_eq!(subagent.tool_calls, 1);
        // Everything else is counted as before.
        assert_eq!((session.turns, session.tool_calls), (2, 4));
    }

    let two = folded(SessionStore::new().with_trace_interactions(2), false);
    assert_eq!(two.trace.len(), 2);
    assert_eq!(two.subagents[AGENT].thread.spans.len(), 3);

    // No trace at all.
    let none = folded(SessionStore::new().with_trace_interactions(0), false);
    assert!(none.trace.is_empty());
    assert!(none.subagents[AGENT].thread.spans.is_empty());
    assert_eq!((none.turns, none.tool_calls), (2, 4));
}

/// A minimal record of the main thread.
fn record(json: &str) -> Record {
    Record::parse(json)
}

fn tool_use(uuid: &str, message: &str, tool: &str, time: &str) -> Record {
    record(&format!(
        r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"2026-11-04T{time}Z","message":{{"id":"{message}","model":"claude-fable-5-2","content":[{{"type":"tool_use","id":"{tool}","name":"Read","input":{{"file_path":"/work/a"}}}}]}}}}"#
    ))
}

#[test]
fn records_with_missing_keys_do_not_break_the_tree() {
    let mut store = SessionStore::new();
    let records = [
        // A response before any prompt: the transcript was read from the
        // middle. It has no `requestRef`, so no start of its own.
        tool_use("x1", "msg_x1", "toolu_x1", "13:00:01.000"),
        // A result for a call that was never read, and one without an id.
        record(
            r#"{"type":"user","uuid":"x2","timestamp":"2026-11-04T13:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_unknown"},{"type":"tool_result"}]}}"#,
        ),
        // A prompt without `promptId`: its `uuid` names the interaction.
        record(
            r#"{"type":"user","uuid":"x3","timestamp":"2026-11-04T13:01:00.000Z","message":{"content":"a prompt without an id"}}"#,
        ),
        // A response without a message id, and a tool block without an id.
        record(
            r#"{"type":"assistant","uuid":"x4","timestamp":"2026-11-04T13:01:02.000Z","requestRef":"ref-unknown","message":{"content":[{"type":"tool_use","name":"Read"}]}}"#,
        ),
        // An interaction duration without a duration, twice.
        record(
            r#"{"type":"system","subtype":"turn_duration","uuid":"x5","timestamp":"2026-11-04T13:01:03.000Z"}"#,
        ),
        record(
            r#"{"type":"system","subtype":"turn_duration","uuid":"x6","timestamp":"2026-11-04T13:01:04.000Z"}"#,
        ),
        record("not json"),
        record(r#"{"type":"never-seen-before"}"#),
    ];
    store.apply_records(ID, &records);
    let session = store.get(ID).unwrap();

    let ids: Vec<&str> = session
        .trace
        .iter()
        .map(|interaction| interaction.id.as_str())
        .collect();
    assert_eq!(ids, ["", "x3"]);
    assert_eq!(session.trace[0].prompt, None, "its prompt was not read");
    assert_eq!(
        session.trace[1].prompt.as_deref(),
        Some("a prompt without an id")
    );
    // The first response starts where its interaction does: nothing was before it.
    assert_eq!(
        tree(session, 0),
        ["llm_request ? ? None", "tool Read None None"]
    );
    // The second starts at the record before it.
    assert_eq!(tree(session, 1), ["llm_request ? ? Some(2000)"]);
    let second = &session.trace[1];
    assert_eq!(second.ended_at, at("13:01:03.000"), "the first end stands");
    assert_eq!(second.duration_ms, None);
}

#[test]
fn a_thread_keeps_a_bounded_number_of_spans() {
    let mut store = SessionStore::new();
    let calls = MAX_SPANS as u64 + 10;
    for index in 0..calls {
        store.apply(
            ID,
            &tool_use(
                &format!("u{index}"),
                &format!("msg_{index}"),
                &format!("toolu_{index}"),
                "14:00:00.000",
            ),
        );
    }
    let session = store.get(ID).unwrap();
    let thread = &session.trace[0].thread;
    assert_eq!(thread.spans.len(), MAX_SPANS);
    // Each record was a request and a call.
    assert_eq!(thread.dropped, calls * 2 - MAX_SPANS as u64);
    assert_eq!(session.walk(&session.trace[0]).len(), MAX_SPANS);
    assert_eq!(session.tool_calls, calls);
}

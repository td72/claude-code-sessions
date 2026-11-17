//! `apply_event` against the event samples and the synthetic transcripts.
//! The samples describe the session of `tool-call.jsonl` and its subagent.

use std::fs;
use std::path::{Path, PathBuf};

use iktinos_core::record::{Record, Timestamp, KNOWN_CLAUDE_CODE_VERSION};
use iktinos_core::session::{LiveState, Session, SessionStore};
use iktinos_live::event::Event;
use iktinos_live::merge::apply_event;

const ID: &str = "22222222-2222-4222-8222-222222222222";
const T0: i64 = 1_793_613_600_000;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn sample(name: &str) -> Event {
    let path = fixtures().join("events").join(format!("{name}.json"));
    Event::parse(&fs::read(path).unwrap()).unwrap()
}

fn event(json: &str) -> Event {
    Event::parse(json.as_bytes()).unwrap()
}

fn records(name: &str) -> Vec<Record> {
    let path = fixtures().join(KNOWN_CLAUDE_CODE_VERSION).join(name);
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(Record::parse)
        .collect()
}

fn apply(store: &mut SessionStore, names: &[&str]) {
    for name in names {
        apply_event(store, &sample(name));
    }
}

fn at(offset_ms: i64) -> Timestamp {
    Timestamp::from_millis(T0 + offset_ms)
}

/// The samples in the order they happened.
const TIMELINE: &[&str] = &[
    "session-start",
    "turn-start",
    "tool-check-allow",
    "tool-call-start",
    "agent-spawn",
    "tool-call-end",
    "subagent-tool-call-start",
    "session-measure",
    "turn-complete",
];

#[test]
fn events_only() {
    let mut store = SessionStore::new();

    apply(&mut store, &["session-start"]);
    let session = store.get(ID).unwrap();
    let live = session.live.as_ref().unwrap();
    assert_eq!(live.cwd.as_deref(), Some(Path::new("/work/other.repo")));
    assert_eq!(live.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(live.state(), LiveState::Unknown);

    apply(&mut store, &["turn-start"]);
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert_eq!(live.state(), LiveState::Working);
    assert_eq!(live.turn.as_ref().unwrap().id, "turn-1");
    assert_eq!(live.turn.as_ref().unwrap().started_at, at(100));

    apply(&mut store, &["tool-check-allow", "tool-call-start"]);
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    let LiveState::RunningTool(tool) = live.state() else {
        panic!("{:?}", live.state())
    };
    assert_eq!(tool.name, "Bash");
    assert_eq!(tool.summary.as_deref(), Some("cargo test --workspace"));
    assert_eq!(tool.started_at, at(2000));

    apply(&mut store, &["agent-spawn", "tool-call-end"]);
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert!(live.running_tools.is_empty());
    assert_eq!(live.state(), LiveState::Working);

    apply(&mut store, &["subagent-tool-call-start", "session-measure"]);
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    let LiveState::RunningTool(tool) = live.state() else {
        panic!()
    };
    assert_eq!(tool.agent_id.as_deref(), Some("a1b2c3d4e5f60718"));
    let context = live.context.unwrap();
    // The window in effect, not the model's largest.
    assert_eq!((context.tokens, context.window), (6716, Some(200_000)));
    assert_eq!(live.rate_limits.len(), 2);
    assert_eq!(live.rate_limits[0].kind, "five_hour");
    assert_eq!(live.rate_limits[0].percent_used, 42.5);
    assert_eq!(live.measured_at, Some(at(8050)));

    apply(&mut store, &["turn-complete"]);
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    // The main turn is over; the subagent's call is its own business.
    assert_eq!(live.turn, None);
    assert_eq!(live.idle_since, Some(at(8100)));
    assert_eq!(live.running_tools.len(), 1);
    apply_event(
        &mut store,
        &event(&format!(
            r#"{{"v":1,"type":"turn.complete","session_id":"{ID}","ts":{},"turn_id":"sub-1",
                "agent_id":"a1b2c3d4e5f60718","reason":"answer"}}"#,
            T0 + 8200
        )),
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert_eq!(live.state(), LiveState::WaitingForInput);

    apply(&mut store, &["session-end"]);
    let session = store.get(ID).unwrap();
    assert_eq!(session.live.as_ref().unwrap().state(), LiveState::Ended);

    // Nothing the transcripts own was written.
    assert!(session.usage.by_model.is_empty());
    assert_eq!(session.cost.usd, None);
    assert_eq!((session.turns, session.tool_calls), (0, 0));
    assert_eq!(session.model, None);
    assert_eq!(session.cwd, None);
    assert_eq!(session.context, None);
    assert!(!session.exited);
}

#[test]
fn a_permission_prompt_is_waiting_for_approval() {
    let mut store = SessionStore::new();
    apply(
        &mut store,
        &["session-start", "turn-start", "tool-check-ask"],
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    let LiveState::AwaitingApproval(pending) = live.state() else {
        panic!("{:?}", live.state())
    };
    assert_eq!(pending.name, "Write");
    assert_eq!(pending.since, at(1950));

    // Approved: the call starts, and the wait is over.
    apply_event(
        &mut store,
        &event(&format!(
            r#"{{"v":1,"type":"tool.call","session_id":"{ID}","ts":{},"phase":"start",
                "tool":"Write","tool_use_id":"toolu_09"}}"#,
            T0 + 4000
        )),
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert_eq!(live.awaiting_approval, None);
    assert!(matches!(live.state(), LiveState::RunningTool(t) if t.name == "Write"));

    // Refused instead: back to working, nothing runs.
    let mut store = SessionStore::new();
    apply(&mut store, &["turn-start", "tool-check-ask"]);
    apply_event(
        &mut store,
        &event(&format!(
            r#"{{"v":1,"type":"tool.check","session_id":"{ID}","ts":{},"tool":"Write",
                "tool_use_id":"toolu_09","decision":"deny"}}"#,
            T0 + 4000
        )),
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert_eq!(live.state(), LiveState::Working);
}

/// The two inputs in either order end in the same session.
fn merged(records_first: bool) -> Session {
    let mut store = SessionStore::new();
    let fold = |store: &mut SessionStore| {
        store.apply_records(ID, &records("tool-call.jsonl"));
        store.apply_records(ID, &records("subagent.jsonl"));
    };
    if records_first {
        fold(&mut store);
        apply(&mut store, TIMELINE);
    } else {
        apply(&mut store, TIMELINE);
        fold(&mut store);
    }
    store.get(ID).unwrap().clone()
}

#[test]
fn records_then_events_equals_events_then_records() {
    let (records_first, events_first) = (merged(true), merged(false));
    assert_eq!(records_first, events_first);

    let session = records_first;
    // The transcripts' side is what a fold alone gives.
    let mut alone = SessionStore::new();
    alone.apply_records(ID, &records("tool-call.jsonl"));
    alone.apply_records(ID, &records("subagent.jsonl"));
    let alone = alone.get(ID).unwrap();
    assert_eq!(session.usage, alone.usage);
    assert_eq!(session.cost, alone.cost);
    assert_eq!(session.recent_tools, alone.recent_tools);
    assert_eq!(session.context, alone.context);
    assert_eq!(session.subagents, alone.subagents);
    assert_eq!(session.model, alone.model);
    assert_eq!(
        (session.turns, session.tool_calls, session.exited),
        (alone.turns, alone.tool_calls, alone.exited)
    );
    assert_eq!(alone.live, None);

    // The live side is what the events alone give.
    let mut events = SessionStore::new();
    apply(&mut events, TIMELINE);
    assert_eq!(session.live, events.get(ID).unwrap().live);

    // The transcript knows the model's largest window; the live measurement
    // knows the one in effect, and is preferred.
    assert_eq!(session.context.unwrap().window, Some(1_000_000));
    assert_eq!(session.current_context().unwrap().window, Some(200_000));
    assert_eq!(alone.current_context(), alone.context);
}

#[test]
fn an_older_event_does_not_overwrite_a_newer_state() {
    let mut store = SessionStore::new();
    apply(&mut store, &["turn-start", "turn-complete"]);
    let settled = store.get(ID).unwrap().live.clone();
    assert_eq!(
        settled.as_ref().unwrap().state(),
        LiveState::WaitingForInput
    );

    // Late arrivals from before the turn completed.
    apply(
        &mut store,
        &[
            "tool-check-ask",
            "tool-call-start",
            "turn-start",
            "session-start",
        ],
    );
    assert_eq!(store.get(ID).unwrap().live, settled);

    // An older measurement loses to a newer one too.
    apply(&mut store, &["session-measure"]);
    apply_event(
        &mut store,
        &event(&format!(
            r#"{{"v":1,"type":"session.measure","session_id":"{ID}","ts":{},
                "context":{{"tokens":1,"window":200000}}}}"#,
            T0 + 10
        )),
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert_eq!(live.context.unwrap().tokens, 6716);
}

#[test]
fn a_tool_end_applies_even_when_it_arrives_late() {
    let mut store = SessionStore::new();
    apply(&mut store, &["turn-start", "tool-call-start"]);
    // A later event overtakes the end of the call.
    apply(&mut store, &["subagent-tool-call-start"]);
    assert_eq!(
        store
            .get(ID)
            .unwrap()
            .live
            .as_ref()
            .unwrap()
            .running_tools
            .len(),
        2
    );

    apply(&mut store, &["tool-call-end"]);
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    let names: Vec<_> = live.running_tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Grep"]);
    // The clock did not go back.
    assert_eq!(live.state_at, Some(at(5000)));

    // And a start that arrives after its own end does not bring it back.
    let mut store = SessionStore::new();
    apply(
        &mut store,
        &["turn-start", "tool-call-end", "tool-call-start"],
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert!(live.running_tools.is_empty());
    assert_eq!(live.state(), LiveState::Working);
}

#[test]
fn a_provisional_session_is_joined_by_its_transcript() {
    let mut store = SessionStore::new();
    assert!(store.is_empty());

    // The mod reports a session before its transcript exists.
    apply(&mut store, &["session-start", "turn-start"]);
    assert_eq!(store.len(), 1);
    let provisional = store.get(ID).unwrap();
    assert_eq!(provisional.title, None);
    assert_eq!(provisional.last_activity, None);
    assert_eq!(
        provisional.live.as_ref().unwrap().cwd.as_deref(),
        Some(Path::new("/work/other.repo"))
    );

    // The transcript appears under the same id: one session, both sides.
    store.apply_records(ID, &records("tool-call.jsonl"));
    assert_eq!(store.len(), 1);
    let session = store.get(ID).unwrap();
    assert_eq!(session.usage.total().responses, 3);
    assert_eq!(session.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(session.live.as_ref().unwrap().state(), LiveState::Working);

    // A restart of the transcript forgets the fold, not the live side.
    store.reset(ID);
    let session = store.get(ID).unwrap();
    assert!(session.usage.by_model.is_empty());
    assert_eq!(session.live.as_ref().unwrap().state(), LiveState::Working);
}

#[test]
fn unknown_events_change_nothing() {
    let mut store = SessionStore::new();
    apply(&mut store, &["unknown-type"]);
    // Not even a session.
    assert!(store.is_empty());

    apply(&mut store, &["turn-start"]);
    let before = store.get(ID).unwrap().clone();
    apply(&mut store, &["unknown-type", "agent-spawn"]);
    apply_event(
        &mut store,
        &event(&format!(
            r#"{{"v":1,"type":"tool.call","session_id":"{ID}","ts":{},"phase":"progress","tool":"Bash"}}"#,
            T0 + 2600
        )),
    );
    let after = store.get(ID).unwrap();
    assert_eq!(after.live.as_ref().unwrap().state(), LiveState::Working);
    assert_eq!(
        after.live.as_ref().unwrap().turn,
        before.live.as_ref().unwrap().turn
    );
    assert!(after.live.as_ref().unwrap().running_tools.is_empty());
}

#[test]
fn a_mod_reload_in_the_middle_of_a_turn_keeps_the_turn() {
    let mut store = SessionStore::new();
    apply(
        &mut store,
        &["session-start", "turn-start", "tool-call-start"],
    );
    // `session.start` fires again when the mod reloads.
    apply_event(
        &mut store,
        &event(&format!(
            r#"{{"v":1,"type":"session.start","session_id":"{ID}","ts":{},"cwd":"/work/other.repo"}}"#,
            T0 + 2500
        )),
    );
    let live = store.get(ID).unwrap().live.as_ref().unwrap();
    assert!(matches!(live.state(), LiveState::RunningTool(_)));
    // A start without a model keeps the one already known.
    assert_eq!(live.model.as_deref(), Some("claude-fable-5-2"));
}

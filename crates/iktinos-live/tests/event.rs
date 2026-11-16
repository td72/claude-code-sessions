//! The event samples under `tests/fixtures/events/` are the protocol's
//! contract: every one must survive a round trip unchanged.

use std::fs;
use std::path::PathBuf;

use iktinos_live::event::{Body, Decision, Event, EventError, Phase, TurnEnd, VERSION};
use serde_json::Value;

fn samples() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/events");
    let mut samples: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .map(|path| {
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            (name, fs::read(path).unwrap())
        })
        .collect();
    samples.sort();
    samples
}

fn sample(name: &str) -> Event {
    let (_, bytes) = samples()
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no sample named {name}"));
    Event::parse(&bytes).unwrap()
}

#[test]
fn every_sample_round_trips() {
    let samples = samples();
    assert!(samples.len() >= 12);
    for (name, bytes) in samples {
        let event = Event::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(event.v, VERSION, "{name}");
        if name == "unknown-type" {
            continue;
        }
        assert_ne!(event.body, Body::Unknown, "{name}");
        // Nothing in the sample is lost or renamed on the way back out.
        let written = serde_json::to_value(&event).unwrap();
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(written, original, "{name}");
        let again = Event::parse(&serde_json::to_vec(&event).unwrap()).unwrap();
        assert_eq!(again, event, "{name}");
    }
}

#[test]
fn every_event_type_has_a_sample() {
    let mut types: Vec<String> = samples()
        .iter()
        .map(|(_, bytes)| {
            let value: Value = serde_json::from_slice(bytes).unwrap();
            value["type"].as_str().unwrap().to_owned()
        })
        .collect();
    types.sort();
    types.dedup();
    assert_eq!(
        types,
        [
            "agent.spawn",
            "hologram.frame",
            "session.end",
            "session.measure",
            "session.start",
            "tool.call",
            "tool.check",
            "turn.complete",
            "turn.start",
        ]
    );
}

#[test]
fn the_samples_say_what_they_should() {
    let start = sample("session-start");
    assert_eq!(start.session_id, "22222222-2222-4222-8222-222222222222");
    assert_eq!(start.timestamp().as_millis(), 1_793_613_600_000);
    let Body::SessionStart(body) = start.body else {
        panic!()
    };
    assert_eq!(body.cwd, "/work/other.repo");
    assert_eq!(body.model.as_deref(), Some("claude-fable-5-2"));

    let Body::ToolCall(call) = sample("tool-call-start").body else {
        panic!()
    };
    assert_eq!((call.phase, call.tool.as_str()), (Phase::Start, "Bash"));
    assert_eq!(call.summary.as_deref(), Some("cargo test --workspace"));
    assert_eq!((call.duration_ms, call.success), (None, None));

    let Body::ToolCall(call) = sample("tool-call-end").body else {
        panic!()
    };
    assert_eq!(call.phase, Phase::End);
    assert_eq!((call.duration_ms, call.success), (Some(1500), Some(true)));

    let Body::ToolCall(call) = sample("subagent-tool-call-start").body else {
        panic!()
    };
    assert_eq!(call.agent_id.as_deref(), Some("a1b2c3d4e5f60718"));

    let Body::ToolCheck(check) = sample("tool-check-ask").body else {
        panic!()
    };
    assert_eq!(check.decision, Decision::Ask);

    let Body::SessionMeasure(measure) = sample("session-measure").body else {
        panic!()
    };
    let context = measure.context.unwrap();
    assert_eq!((context.tokens, context.window), (Some(6716), 200_000));
    assert_eq!(measure.rate_limits.len(), 2);
    assert_eq!(measure.rate_limits[0].kind, "five_hour");
    assert_eq!(measure.rate_limits[0].percent_used, 42.5);
    assert_eq!(measure.rate_limits[1].resets_at, None);
    assert_eq!(measure.cost_usd, Some(0.03));

    let Body::TurnComplete(turn) = sample("turn-complete").body else {
        panic!()
    };
    assert_eq!(
        (turn.reason, turn.duration_ms),
        (TurnEnd::Answer, Some(8000))
    );

    let Body::AgentSpawn(spawn) = sample("agent-spawn").body else {
        panic!()
    };
    assert_eq!(spawn.subagent_type, "Explore");
    assert_eq!(spawn.agent_id.as_deref(), Some("a1b2c3d4e5f60718"));
}

#[test]
fn an_unknown_type_is_accepted_as_unknown() {
    let event = sample("unknown-type");
    assert_eq!(event.body, Body::Unknown);
    // The envelope is still read.
    assert_eq!(event.session_id, "22222222-2222-4222-8222-222222222222");
    assert_eq!(event.ts, 1_793_613_609_500);
}

#[test]
fn what_a_newer_sender_adds_is_tolerated() {
    let event = Event::parse(
        br#"{"v":1,"type":"tool.check","session_id":"s","ts":1,"tool":"Bash",
            "decision":"escalate","added_later":{"x":1}}"#,
    )
    .unwrap();
    let Body::ToolCheck(check) = event.body else {
        panic!()
    };
    assert_eq!(check.decision, Decision::Unknown);

    let event = Event::parse(
        br#"{"v":1,"type":"tool.call","session_id":"s","ts":1,"tool":"Bash","phase":"progress"}"#,
    )
    .unwrap();
    assert!(matches!(event.body, Body::ToolCall(c) if c.phase == Phase::Unknown));

    let event = Event::parse(
        br#"{"v":1,"type":"turn.complete","session_id":"s","ts":1,"turn_id":"t","reason":"timeout"}"#,
    )
    .unwrap();
    assert!(matches!(event.body, Body::TurnComplete(t) if t.reason == TurnEnd::Unknown));
}

#[test]
fn what_is_not_an_event_is_refused() {
    for bytes in [
        &b"not json"[..],
        b"[]",
        b"{}",
        br#"{"v":1,"type":"turn.start","ts":1,"turn_id":"t"}"#,
        br#"{"v":1,"type":"turn.start","session_id":"s","turn_id":"t"}"#,
        br#"{"v":1,"type":"turn.start","session_id":"s","ts":1}"#,
        br#"{"v":1,"session_id":"s","ts":1}"#,
    ] {
        assert!(
            matches!(Event::parse(bytes), Err(EventError::Json(_))),
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
    assert!(matches!(
        Event::parse(br#"{"v":2,"type":"turn.start","session_id":"s","ts":1,"turn_id":"t"}"#),
        Err(EventError::UnsupportedVersion(2))
    ));
}

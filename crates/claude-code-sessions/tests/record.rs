//! The `record` views and the drift survey against the synthetic fixtures.

use std::fs;
use std::path::PathBuf;

use claude_code_sessions::record::drift::Drift;
use claude_code_sessions::record::{
    Assistant, Record, Timestamp, TitleSource, Usage, View, KNOWN_CLAUDE_CODE_VERSION,
};

/// Every text field the views do not model (responses, tool inputs and
/// outputs, prompts in `user` records) carries this marker, so a test can
/// assert that only modelled text reaches a view, a `Debug` or a report.
const TEXT_MARKER: &str = "SYNTHETIC-TEXT";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
}

fn load(name: &str) -> Vec<Record> {
    let path = fixture_dir().join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(Record::parse)
        .collect()
}

fn kinds(records: &[Record]) -> Vec<&'static str> {
    records
        .iter()
        .map(|r| match r.view() {
            View::User(_) => "user",
            View::Assistant(_) => "assistant",
            View::System(_) => "system",
            View::Title(_) => "title",
            View::LastPrompt(_) => "last-prompt",
            View::ApiRequest(_) => "api-request",
            View::Other => "other",
            View::Unknown => "unknown",
            View::Invalid => "invalid",
            _ => "unmatched",
        })
        .collect()
}

fn assistant(record: &Record) -> &Assistant {
    match record.view() {
        View::Assistant(a) => a,
        other => panic!("expected an assistant record, got {other:?}"),
    }
}

#[test]
fn plain_turn() {
    let records = load("plain-turn.jsonl");
    assert_eq!(
        kinds(&records),
        [
            "other",
            "user",
            "api-request",
            "other",
            "assistant",
            "assistant",
            "system",
            "title",
            "last-prompt",
        ]
    );

    let prompt = records[1].common();
    assert_eq!(prompt.kind.as_deref(), Some("user"));
    assert_eq!(prompt.uuid.as_deref(), Some("u1"));
    assert_eq!(prompt.parent_uuid, None);
    assert_eq!(
        prompt.session_id.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(
        prompt.timestamp,
        Timestamp::parse("2026-11-01T09:00:00.000Z")
    );
    assert_eq!(prompt.cwd, Some(PathBuf::from("/work/example")));
    assert_eq!(prompt.git_branch.as_deref(), Some("main"));
    assert_eq!(prompt.version.as_deref(), Some(KNOWN_CLAUDE_CODE_VERSION));
    assert_eq!(prompt.is_sidechain, Some(false));
    assert_eq!(prompt.agent_id, None);
    let View::User(user) = records[1].view() else {
        unreachable!()
    };
    assert!(user.tool_results.is_empty());

    let View::ApiRequest(request) = records[2].view() else {
        unreachable!()
    };
    assert_eq!(request.id.as_deref(), Some("ref-1"));
    assert_eq!(request.query_source.as_deref(), Some("repl_main_thread"));

    // One response, two records (thinking, text): same message, same usage.
    let (thinking, text) = (assistant(&records[4]), assistant(&records[5]));
    assert_eq!(thinking.message_id.as_deref(), Some("msg_1"));
    assert_eq!(thinking.message_id, text.message_id);
    assert_eq!(thinking.usage, text.usage);
    assert_eq!((thinking.block_index, text.block_index), (Some(0), Some(1)));
    assert_eq!(text.request_ref.as_deref(), Some("ref-1"));
    assert_eq!(text.request_id.as_deref(), Some("req_ref-1"));
    assert_eq!(text.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(text.stop_reason.as_deref(), Some("end_turn"));
    assert!(text.tool_uses.is_empty());
    assert!(!text.is_api_error);
    let usage = text.usage.unwrap();
    assert_eq!(
        usage,
        Usage {
            input_tokens: 12,
            output_tokens: 80,
            cache_creation_input_tokens: 300,
            cache_read_input_tokens: 4000,
            cache_creation_5m_input_tokens: Some(0),
            cache_creation_1h_input_tokens: Some(300),
        }
    );
    assert_eq!(usage.context_tokens(), 4312);

    let View::System(system) = records[6].view() else {
        unreachable!()
    };
    assert_eq!(system.subtype.as_deref(), Some("turn_duration"));
    assert_eq!(system.duration_ms, Some(4000));
    assert_eq!(system.compact, None);

    let View::Title(title) = records[7].view() else {
        unreachable!()
    };
    assert_eq!(title.title.as_deref(), Some("Synthetic session title"));
    assert_eq!(title.source, TitleSource::Ai);

    let View::LastPrompt(last) = records[8].view() else {
        unreachable!()
    };
    assert_eq!(last.leaf_uuid.as_deref(), Some("a2"));
    assert_eq!(last.text.as_deref(), Some("Synthetic last prompt"));
}

#[test]
fn tool_calls_and_their_results() {
    let records = load("tool-call.jsonl");
    assert_eq!(
        kinds(&records),
        [
            "user",
            "api-request",
            "assistant",
            "assistant",
            "user",
            "user",
            "api-request",
            "assistant",
            "user",
            "api-request",
            "assistant",
            "system",
            "other",
        ]
    );

    let calls: Vec<(&str, &str)> = records
        .iter()
        .filter_map(|r| match r.view() {
            View::Assistant(a) => Some(a),
            _ => None,
        })
        .flat_map(|a| &a.tool_uses)
        .map(|t| (t.id.as_deref().unwrap(), t.name.as_deref().unwrap()))
        .collect();
    assert_eq!(
        calls,
        [
            ("toolu_01", "Bash"),
            ("toolu_02", "Read"),
            ("toolu_03", "mcp__example__lookup"),
        ]
    );

    // `is_error`: false, true, and absent (which is a success).
    let results: Vec<(&str, Option<bool>, bool)> = records
        .iter()
        .filter_map(|r| match r.view() {
            View::User(u) => Some(u),
            _ => None,
        })
        .flat_map(|u| &u.tool_results)
        .map(|t| (t.tool_use_id.as_deref().unwrap(), t.is_error, t.success()))
        .collect();
    assert_eq!(
        results,
        [
            ("toolu_01", Some(false), true),
            ("toolu_02", Some(true), false),
            ("toolu_03", None, true),
        ]
    );

    // A call's duration is the gap between its record and its result's.
    let called = records[2].common().timestamp.unwrap();
    let answered = records[4].common().timestamp.unwrap();
    assert_eq!(answered.as_millis() - called.as_millis(), 1500);
}

#[test]
fn subagent_file() {
    let records = load("subagent.jsonl");
    assert_eq!(
        kinds(&records),
        [
            "user",
            "api-request",
            "assistant",
            "user",
            "api-request",
            "assistant"
        ]
    );
    for record in &records {
        let common = record.common();
        assert_eq!(common.agent_id.as_deref(), Some("a1b2c3d4e5f60718"));
        // A subagent writes under its parent's session id.
        assert_eq!(
            common.session_id.as_deref(),
            Some("22222222-2222-4222-8222-222222222222")
        );
        if !matches!(record.view(), View::ApiRequest(_)) {
            assert_eq!(common.is_sidechain, Some(true));
        }
    }
    let View::ApiRequest(request) = records[1].view() else {
        unreachable!()
    };
    assert_eq!(
        request.query_source.as_deref(),
        Some("agent:builtin:Explore")
    );
    assert_eq!(
        assistant(&records[2]).model.as_deref(),
        Some("claude-haiku-5-5")
    );
}

#[test]
fn malformed_lines_do_not_fail_the_file() {
    let records = load("malformed.jsonl");
    assert_eq!(
        kinds(&records),
        ["user", "invalid", "invalid", "invalid", "title"]
    );
    for record in &records[1..4] {
        assert_eq!(record.kind(), None);
        assert_eq!(record.common().session_id, None);
    }

    let drift = Drift::survey(&records);
    assert_eq!((drift.records, drift.invalid), (5, 3));
    assert!(drift.is_clean());
}

#[test]
fn the_modelled_fixtures_have_no_drift() {
    for name in ["plain-turn.jsonl", "tool-call.jsonl", "subagent.jsonl"] {
        let drift = Drift::survey(&load(name));
        assert!(drift.is_clean(), "{name}:\n{drift}");
        assert_eq!(drift.invalid, 0, "{name}");
        assert!(drift.unchecked_versions().is_empty(), "{name}");
        assert!(drift.versions.contains_key(KNOWN_CLAUDE_CODE_VERSION));
    }
}

#[test]
fn the_drift_survey_reports_what_was_planted() {
    let records = load("unknown.jsonl");
    assert_eq!(
        kinds(&records),
        ["unknown", "assistant", "other", "system", "unknown"]
    );
    // An unknown record is kept whole, not dropped.
    assert_eq!(records[0].kind(), Some("hologram-frame"));
    assert!(records[0].raw().get("payload").is_some());

    let drift = Drift::survey(&records);
    assert!(!drift.is_clean());
    let names = |map: &std::collections::BTreeMap<String, u64>| -> Vec<String> {
        map.keys().cloned().collect()
    };
    assert_eq!(names(&drift.unknown_types), ["<none>", "hologram-frame"]);
    assert_eq!(
        names(&drift.unknown_keys),
        [
            "assistant.futureKey",
            "assistant.message.futureMessageKey",
            "assistant.message.usage.quantum_tokens",
        ]
    );
    assert_eq!(names(&drift.unknown_block_types), ["assistant.hologram"]);
    assert_eq!(
        names(&drift.unknown_attachment_types),
        ["future_attachment"]
    );
    assert_eq!(names(&drift.unknown_system_subtypes), ["future_subtype"]);
    assert_eq!(drift.unchecked_versions(), ["9.0.0"]);

    let report = drift.to_string();
    for expected in [
        "hologram-frame ×1",
        "assistant.message.usage.quantum_tokens ×1",
        "assistant.hologram ×1",
        "future_attachment ×1",
        "future_subtype ×1",
        "newer than checked: 9.0.0",
    ] {
        assert!(
            report.contains(expected),
            "missing {expected:?} in\n{report}"
        );
    }
}

#[test]
fn unmodelled_text_stays_in_the_raw_value() {
    let mut seen_in_raw = false;
    for entry in fs::read_dir(fixture_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let records = load(&name);
        for record in &records {
            seen_in_raw |= record.raw().to_string().contains(TEXT_MARKER);
            let shown = format!("{record:?} {:?} {:?}", record.view(), record.common());
            assert!(!shown.contains(TEXT_MARKER), "{name}: {shown}");
        }
        let drift = Drift::survey(&records);
        let shown = format!("{drift} {drift:?}");
        assert!(!shown.contains(TEXT_MARKER), "{name}: {shown}");
    }
    // The assertion above only means something if the fixtures do carry text.
    assert!(seen_in_raw);
}

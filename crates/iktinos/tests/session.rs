//! The fold of the synthetic fixtures into `Session`s.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iktinos::discover::discover;
use iktinos::record::{Record, Timestamp, KNOWN_CLAUDE_CODE_VERSION};
use iktinos::session::{Activity, CostSource, Session, SessionStore};

const ID: &str = "session-under-test";

// Price-table estimates of the fixtures, in USD (see tests/fixtures/README.md
// for what each file holds).
const PLAIN_TURN: f64 = 0.008896; // one claude-fable-5-2 response
const TOOL_CALLS: f64 = 0.019256; // three claude-fable-5-2 responses
const SUBAGENT: f64 = 0.0002388; // two claude-haiku-5-5 responses

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
}

fn load(name: &str) -> Vec<Record> {
    fs::read_to_string(fixture_dir().join(name))
        .unwrap()
        .lines()
        .map(Record::parse)
        .collect()
}

fn folded(files: &[&str]) -> Session {
    let mut store = SessionStore::new();
    for file in files {
        store.apply_records(ID, &load(file));
    }
    store.get(ID).unwrap().clone()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

#[test]
fn records_only() {
    let session = folded(&["plain-turn.jsonl"]);
    assert_eq!(session.id, ID);
    assert_eq!(session.cwd.as_deref(), Some(Path::new("/work/example")));
    assert_eq!(session.git_branch.as_deref(), Some("main"));
    assert_eq!(session.version.as_deref(), Some(KNOWN_CLAUDE_CODE_VERSION));
    assert_eq!(session.title.as_deref(), Some("Synthetic session title"));
    assert_eq!(
        session.last_prompt.as_deref(),
        Some("Synthetic last prompt")
    );
    assert_eq!(session.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(session.started_at, Some(at("2026-11-01T09:00:00.000Z")));
    assert_eq!(session.last_activity, Some(at("2026-11-01T09:00:04.100Z")));
    assert!(!session.exited);
    assert_eq!(session.turns, 1);
    assert_eq!(session.tool_calls, 0);
    assert!(session.subagents.is_empty());

    // The response is two records (thinking, text) with the same usage: it
    // counts once.
    let usage = session.usage.by_model["claude-fable-5-2"];
    assert_eq!(usage.responses, 1);
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 80);
    assert_eq!(usage.cache_write_5m_tokens, 0);
    assert_eq!(usage.cache_write_1h_tokens, 300);
    assert_eq!(usage.cache_read_tokens, 4000);
    assert_eq!(usage.total_tokens(), 4392);
    assert!(close(usage.estimated_cost().unwrap(), PLAIN_TURN));
    assert_eq!(session.usage.total(), usage);
    assert_eq!(session.usage.by_query_source["repl_main_thread"], usage);
    assert_eq!(session.usage.by_query_source.len(), 1);

    let context = session.context.unwrap();
    assert_eq!(context.tokens, 4312);
    assert_eq!(context.window, Some(1_000_000));
    assert!(close(context.fill().unwrap(), 0.004312));

    // Nothing reported by Claude Code: the cost is the estimate.
    assert!(close(session.cost.usd.unwrap(), PLAIN_TURN));
    assert_eq!(session.cost.source, CostSource::Estimated);
    assert!(!session.cost.incomplete);
}

#[test]
fn the_same_records_applied_twice_count_once() {
    for files in [
        &["plain-turn.jsonl"][..],
        &["tool-call.jsonl", "subagent.jsonl"],
        &["resumed.jsonl"],
    ] {
        let once = folded(files);
        let twice = folded(&[files, files].concat());
        assert_eq!(once, twice, "{files:?}");
    }

    // A batch that overlaps the previous one, as after a re-read.
    let records = load("resumed.jsonl");
    let mut store = SessionStore::new();
    store.apply_records(ID, &records[..15]);
    store.apply_records(ID, &records[10..]);
    assert_eq!(store.get(ID).unwrap(), &folded(&["resumed.jsonl"]));
}

#[test]
fn tool_calls_carry_name_duration_and_outcome() {
    let session = folded(&["tool-call.jsonl"]);
    assert_eq!(session.tool_calls, 3);
    assert_eq!(session.turns, 1);
    let calls: Vec<_> = session
        .recent_tools
        .iter()
        .map(|c| (c.name.as_str(), c.duration_ms, c.success))
        .collect();
    assert_eq!(
        calls,
        [
            ("Bash", Some(1500), Some(true)),
            ("Read", Some(1200), Some(false)),
            ("mcp__example__lookup", Some(1000), Some(true)),
        ]
    );
    assert!(session.recent_tools.iter().all(|c| c.agent_id.is_none()));
    assert_eq!(session.usage.total().responses, 3);
    assert!(close(session.cost.usd.unwrap(), TOOL_CALLS));
    assert_eq!(session.context.unwrap().tokens, 6716);
}

#[test]
fn a_call_without_its_result_is_still_running() {
    let records = load("tool-call.jsonl");
    let mut store = SessionStore::new();
    // Up to the first two calls, before any result.
    store.apply_records(ID, &records[..4]);
    let session = store.get(ID).unwrap();
    assert_eq!(session.recent_tools.len(), 2);
    assert!(session
        .recent_tools
        .iter()
        .all(|c| c.success.is_none() && c.duration_ms.is_none()));
}

#[test]
fn recent_tool_calls_are_bounded() {
    let mut store = SessionStore::new().with_recent_tools(2);
    store.apply_records(ID, &load("tool-call.jsonl"));
    store.apply_records(ID, &load("subagent.jsonl"));
    let session = store.get(ID).unwrap();
    assert_eq!(session.tool_calls, 4);
    let names: Vec<_> = session
        .recent_tools
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["mcp__example__lookup", "Grep"]);
}

#[test]
fn a_subagent_joins_its_parent() {
    // Whichever transcript is read first, the result is the same.
    let session = folded(&["tool-call.jsonl", "subagent.jsonl"]);
    let reversed = folded(&["subagent.jsonl", "tool-call.jsonl"]);
    assert_eq!(session.subagents, reversed.subagents);
    assert_eq!(
        session.usage.total().total_tokens(),
        reversed.usage.total().total_tokens()
    );
    assert_eq!(
        session.usage.by_query_source.keys().collect::<Vec<_>>(),
        reversed.usage.by_query_source.keys().collect::<Vec<_>>()
    );
    assert!(close(session.cost.usd.unwrap(), reversed.cost.usd.unwrap()));
    assert_eq!(session.cost.source, reversed.cost.source);
    assert_eq!(session.context, reversed.context);
    assert_eq!(session.model, reversed.model);

    assert_eq!(session.subagents.len(), 1);
    let subagent = &session.subagents["a1b2c3d4e5f60718"];
    assert_eq!(subagent.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(subagent.usage.responses, 2);
    assert_eq!(subagent.usage.input_tokens, 40);
    assert_eq!(subagent.usage.cache_write_1h_tokens, 870);
    assert!(close(subagent.usage.estimated_usd, SUBAGENT));
    assert_eq!(subagent.tool_calls, 1);
    assert_eq!(subagent.started_at, Some(at("2026-11-02T10:00:04.000Z")));
    assert_eq!(subagent.last_activity, Some(at("2026-11-02T10:00:06.500Z")));

    // The parent's own fields are the main thread's, not the subagent's.
    assert_eq!(session.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(session.cwd.as_deref(), Some(Path::new("/work/example")));
    assert_eq!(session.context.unwrap().tokens, 6716);
    assert_eq!(session.turns, 1);

    // Its tokens and cost are the session's too.
    assert_eq!(session.tool_calls, 4);
    assert_eq!(session.usage.by_model.len(), 2);
    assert_eq!(session.usage.by_model["claude-haiku-5-5"], subagent.usage);
    assert_eq!(
        session.usage.by_query_source["repl_main_thread"].responses,
        3
    );
    assert_eq!(
        session.usage.by_query_source["agent:builtin:Explore"],
        subagent.usage
    );
    assert!(close(session.cost.usd.unwrap(), TOOL_CALLS + SUBAGENT));
    let grep = session.recent_tools.back().unwrap();
    assert_eq!(grep.name, "Grep");
    assert_eq!(grep.agent_id.as_deref(), Some("a1b2c3d4e5f60718"));
    assert_eq!((grep.duration_ms, grep.success), (Some(400), Some(true)));
}

#[test]
fn an_unknown_model_costs_unknown_not_zero() {
    let session = folded(&["unknown-model.jsonl"]);
    let usage = session.usage.by_model["claude-future-9"];
    assert_eq!(usage.responses, 1);
    assert_eq!(usage.unpriced_responses, 1);
    assert_eq!(usage.output_tokens, 80);
    assert_eq!(usage.estimated_cost(), None);
    assert_eq!(session.usage.total().estimated_cost(), None);

    assert_eq!(session.cost.usd, None);
    assert_eq!(session.cost.source, CostSource::Unknown);
    assert!(session.cost.incomplete);

    let context = session.context.unwrap();
    assert_eq!(context.tokens, 4312);
    assert_eq!(context.window, None);
    assert_eq!(context.fill(), None);

    // Alongside a known model the estimate is partial, and says so.
    let mut store = SessionStore::new();
    store.apply_records(ID, &load("tool-call.jsonl"));
    let future = fs::read_to_string(fixture_dir().join("unknown-model.jsonl"))
        .unwrap()
        // The two fixtures are different sessions that reuse ids.
        .replace(r#"Uuid":""#, r#"Uuid":"future-"#)
        .replace(r#""uuid":""#, r#""uuid":"future-"#)
        .replace("msg_1", "msg_future");
    store.apply_records(ID, &future.lines().map(Record::parse).collect::<Vec<_>>());
    let mixed = store.get(ID).unwrap().clone();
    assert!(close(mixed.cost.usd.unwrap(), TOOL_CALLS));
    assert_eq!(mixed.cost.source, CostSource::Estimated);
    assert!(mixed.cost.incomplete);
    assert!(close(
        mixed.usage.total().estimated_cost().unwrap(),
        TOOL_CALLS
    ));
}

#[test]
fn the_reported_cost_is_used_and_extended_past_the_last_report() {
    let records = load("resumed.jsonl");
    let report = records
        .iter()
        .position(|r| r.kind() == Some("cost-state"))
        .unwrap();

    // Up to the exit: Claude Code's own total ($0.03, above the $0.019256
    // the transcript accounts for) is the cost.
    let mut store = SessionStore::new();
    store.apply_records(ID, &records[..=report]);
    let session = store.get(ID).unwrap();
    assert!(session.exited);
    assert_eq!(session.cost.usd, Some(0.03));
    assert_eq!(session.cost.source, CostSource::Reported);

    // A subagent that ran before the exit is already inside the report,
    // whenever its transcript is read.
    store.apply_records(ID, &load("subagent.jsonl"));
    let session = store.get(ID).unwrap();
    assert_eq!(session.cost.usd, Some(0.03));
    assert_eq!(session.cost.source, CostSource::Reported);

    // Resumed: the turn after the report is estimated on top of it.
    store.apply_records(ID, &records[report + 1..]);
    let session = store.get(ID).unwrap();
    assert!(!session.exited);
    assert!(close(session.cost.usd.unwrap(), 0.03 + PLAIN_TURN));
    assert_eq!(session.cost.source, CostSource::ReportedPlusEstimate);
    assert_eq!(session.turns, 2);
    assert_eq!(
        session.title.as_deref(),
        Some("Synthetic title after the resume")
    );
    assert_eq!(session.last_activity, Some(at("2026-11-05T09:00:04.100Z")));
    // Tokens always come from the transcript.
    assert_eq!(session.usage.total().responses, 6);
}

#[test]
fn a_report_below_what_the_transcript_shows_loses_to_the_estimate() {
    // A killed process never wrote its total; the next exit reports far less
    // than the responses on record.
    let mut store = SessionStore::new();
    store.apply_records(ID, &load("tool-call.jsonl"));
    store.apply(
        ID,
        &Record::parse(r#"{"type":"cost-state","sessionId":"s","totalCostUSD":0.001}"#),
    );
    let session = store.get(ID).unwrap();
    assert!(session.exited);
    assert!(close(session.cost.usd.unwrap(), TOOL_CALLS));
    assert_eq!(session.cost.source, CostSource::Estimated);
}

#[test]
fn a_preview_sets_the_snapshot_fields_and_the_fold_catches_up() {
    let records = load("resumed.jsonl");
    let mut store = SessionStore::new();

    // A first look at the end of the transcript.
    store.preview_records(ID, &records[records.len() - 6..]);
    let preview = store.get(ID).unwrap();
    assert_eq!(
        preview.title.as_deref(),
        Some("Synthetic title after the resume")
    );
    assert_eq!(
        preview.last_prompt.as_deref(),
        Some("Synthetic prompt after the resume")
    );
    assert_eq!(preview.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(preview.git_branch.as_deref(), Some("main"));
    assert_eq!(preview.last_activity, Some(at("2026-11-05T09:00:04.100Z")));
    assert_eq!(preview.context.unwrap().tokens, 4312);
    assert!(!preview.exited);
    // Nothing accumulated yet.
    assert!(preview.usage.by_model.is_empty());
    assert_eq!(preview.cost.usd, None);
    assert_eq!((preview.turns, preview.tool_calls), (0, 0));

    // The full read, from the beginning, ends where a plain fold ends.
    store.apply_records(ID, &records);
    assert_eq!(store.get(ID).unwrap(), &folded(&["resumed.jsonl"]));
}

#[test]
fn a_preview_of_an_exited_session_says_so() {
    let records = load("resumed.jsonl");
    let report = records
        .iter()
        .position(|r| r.kind() == Some("cost-state"))
        .unwrap();
    let mut store = SessionStore::new();
    store.preview_records(ID, &records[report - 3..=report]);
    assert!(store.get(ID).unwrap().exited);
}

#[test]
fn activity_is_what_the_transcript_alone_can_say() {
    let window = Duration::from_secs(30);
    let session = folded(&["plain-turn.jsonl"]);
    // Last record at 09:00:04.100.
    assert_eq!(
        session.activity(at("2026-11-01T09:00:10.100Z"), window),
        Activity::Active {
            quiet: Duration::from_secs(6)
        }
    );
    assert_eq!(
        session.activity(at("2026-11-01T09:10:00.000Z"), window),
        Activity::Idle
    );
    // A clock behind the transcript is "just now", not a panic.
    assert_eq!(
        session.activity(at("2026-11-01T08:00:00.000Z"), window),
        Activity::Active {
            quiet: Duration::ZERO
        }
    );

    let records = load("resumed.jsonl");
    let report = records
        .iter()
        .position(|r| r.kind() == Some("cost-state"))
        .unwrap();
    let mut store = SessionStore::new();
    store.apply_records(ID, &records[..=report]);
    assert_eq!(
        store
            .get(ID)
            .unwrap()
            .activity(at("2026-11-02T10:00:09.000Z"), window),
        Activity::Exited
    );
}

#[test]
fn malformed_and_unknown_records_are_skipped() {
    let session = folded(&["malformed.jsonl", "unknown.jsonl"]);
    assert_eq!(
        session.title.as_deref(),
        Some("Synthetic title after the damage")
    );
    // The one response in `unknown.jsonl` still counts.
    assert_eq!(session.usage.total().responses, 1);
}

#[test]
fn discovery_seeds_sessions_and_a_reset_keeps_what_it_set() {
    const WITH_SUBAGENTS: &str = "22222222-2222-4222-8222-222222222222";
    let found = discover(&fixture_dir().join("projects")).unwrap();
    let mut store = SessionStore::new();
    assert!(store.is_empty());
    for files in &found {
        store.discovered(files);
    }
    assert_eq!(store.len(), 3);
    let ids: Vec<_> = store.sessions().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));

    // Fold every file of the session that has subagents.
    let files = found.iter().find(|f| f.id == WITH_SUBAGENTS).unwrap();
    let read = |path: &Path| -> Vec<Record> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(Record::parse)
            .collect()
    };
    store.apply_records(WITH_SUBAGENTS, &read(&files.transcript.path));
    for subagent in &files.subagents {
        store.apply_records(WITH_SUBAGENTS, &read(&subagent.transcript.path));
    }

    let session = store.get(WITH_SUBAGENTS).unwrap();
    assert_eq!(
        session.project_path.as_deref(),
        Some(Path::new("/work/other.repo"))
    );
    assert_eq!(session.subagents.len(), 2);
    let explore = &session.subagents["a1b2c3d4e5f60718"];
    assert_eq!(explore.agent_type.as_deref(), Some("Explore"));
    assert_eq!(explore.description.as_deref(), Some("Synthetic agent task"));
    assert_eq!(explore.finished, Some(true));
    assert_eq!(explore.usage.responses, 2);
    // The one without a metadata file is known from its records alone.
    let bare = &session.subagents["b2c3d4e5f6071829"];
    assert_eq!(bare.agent_type, None);
    assert_eq!(bare.usage.responses, 2);
    assert!(close(
        session.cost.usd.unwrap(),
        TOOL_CALLS + SUBAGENT + SUBAGENT
    ));

    // A restarted file: forget what was folded, keep what discovery said.
    store.reset(WITH_SUBAGENTS);
    let session = store.get(WITH_SUBAGENTS).unwrap();
    assert!(session.usage.by_model.is_empty());
    assert_eq!(session.cost.usd, None);
    assert_eq!(session.title, None);
    assert_eq!(
        session.project_path.as_deref(),
        Some(Path::new("/work/other.repo"))
    );
    assert_eq!(
        session.subagents["a1b2c3d4e5f60718"].agent_type.as_deref(),
        Some("Explore")
    );
    assert_eq!(session.subagents["a1b2c3d4e5f60718"].usage.responses, 0);

    assert!(store.remove(WITH_SUBAGENTS).is_some());
    assert_eq!(store.len(), 2);
}

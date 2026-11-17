//! What a tool returned: the excerpt a session keeps when asked to, and the
//! whole output read again on demand.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iktinos_core::discover::{discover, SessionFiles};
use iktinos_core::output::{read_tool_output, WholeOutput};
use iktinos_core::record::drift::Drift;
use iktinos_core::record::{Record, ToolOutput, View, KNOWN_CLAUDE_CODE_VERSION, OUTPUT_CHARS};
use iktinos_core::session::{Session, SessionStore, ToolCall};
use iktinos_core::trace::SpanKind;

const ID: &str = "55555555-5555-4555-8555-555555555555";
const MARKER: &str = "SYNTHETIC-TEXT";

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

/// The session whose calls return every kind of output.
fn files() -> SessionFiles {
    let found = discover(&fixture_dir().join("output")).unwrap();
    found.into_iter().find(|files| files.id == ID).unwrap()
}

/// The output over the limit, as the fixture holds it.
fn long_output() -> String {
    let mut text: String = (1..=200)
        .map(|line| format!("line {line:04} of synthetic output\n"))
        .collect();
    text.push_str("SYNTHETIC-TEXT past the limit");
    text
}

fn folded(store: SessionStore) -> Session {
    let mut store = store;
    let files = files();
    store.discovered(&files);
    store.apply_records(ID, &read(&files.transcript.path));
    store.get(ID).unwrap().clone()
}

fn call<'a>(session: &'a Session, id: &str) -> &'a ToolCall {
    session
        .recent_tools
        .iter()
        .find(|call| call.id == id)
        .unwrap_or_else(|| panic!("no call {id}"))
}

#[test]
fn a_result_is_viewed_as_the_start_of_what_it_returned() {
    let records = read(&files().transcript.path);
    let outputs: BTreeMap<String, ToolOutput> = records
        .iter()
        .filter_map(|record| match record.view() {
            View::User(user) => Some(user.tool_results.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|result| Some((result.tool_use_id?, result.output?)))
        .collect();
    assert_eq!(outputs.len(), 7);

    // A success: the text, whole.
    let ok = &outputs["toolu_o1"];
    assert_eq!(ok.text, "Synthetic output: 3 passed\nall green");
    assert_eq!((ok.chars, ok.lines), (ok.text.chars().count(), 2));
    assert_eq!(
        (ok.truncated, ok.interrupted, ok.other_blocks),
        (false, false, 0)
    );
    assert_eq!(ok.stderr, None, "an empty stderr is none");

    // An error says what went wrong.
    assert_eq!(
        outputs["toolu_o2"].text,
        "Error: synthetic failure, no such file"
    );

    // stderr is a field of its own.
    let warned = &outputs["toolu_o3"];
    assert_eq!(warned.text, "Synthetic output with a warning");
    assert_eq!(
        warned.stderr.as_deref(),
        Some("synthetic warning: deprecated flag\nsecond line of stderr")
    );

    // Over the limit: cut, with what the whole was.
    let long = &outputs["toolu_o4"];
    let whole = long_output();
    assert!(long.truncated);
    assert_eq!(long.text.chars().count(), OUTPUT_CHARS);
    assert!(whole.starts_with(&long.text));
    assert_eq!((long.chars, long.lines), (whole.chars().count(), 201));
    assert!(!long.text.contains(MARKER), "the marker is past the limit");

    // A block that is not text is counted, nothing more.
    let shot = &outputs["toolu_o5"];
    assert_eq!(shot.text, "Synthetic caption of a screenshot");
    assert_eq!(shot.other_blocks, 1);

    // Moved to a file: the transcript has what the model was given.
    assert_eq!(
        outputs["toolu_o6"].text,
        "Synthetic preview of an output moved to a file"
    );

    // Interrupted before it wrote anything.
    let stopped = &outputs["toolu_o7"];
    assert!(stopped.interrupted);
    assert_eq!(
        (stopped.text.as_str(), stopped.chars, stopped.lines),
        ("", 0, 0)
    );
}

#[test]
fn debug_shows_no_output() {
    for record in read(&files().transcript.path) {
        let shown = format!("{record:?}");
        for text in [
            "Synthetic output",
            "synthetic warning",
            "Synthetic caption",
            "Synthetic preview",
            "line 0001",
        ] {
            assert!(!shown.contains(text), "{shown}");
        }
    }
    let whole = read_tool_output(&files(), "toolu_o3").unwrap().unwrap();
    let shown = format!("{whole:?}");
    assert!(
        !shown.contains("Synthetic") && !shown.contains("warning"),
        "{shown}"
    );
    // How much there is, is said.
    assert!(shown.contains("stderr: true"), "{shown}");
    let session = folded(SessionStore::new().with_tool_output(OUTPUT_CHARS));
    let shown = format!("{session:?}");
    assert!(!shown.contains("Synthetic output"), "{shown}");
    assert!(shown.contains("lines: 201"), "{shown}");
}

#[test]
fn a_store_keeps_no_output_unless_it_is_told_to() {
    let session = folded(SessionStore::new());
    assert_eq!(session.recent_tools.len(), 7);
    assert!(session
        .recent_tools
        .iter()
        .all(|call| call.output.is_none()));
    for interaction in &session.trace {
        for span in &interaction.thread.spans {
            if let SpanKind::Tool(call) = &span.kind {
                assert!(call.output.is_none());
            }
        }
    }
    // Everything else is as with the output kept.
    let kept = folded(SessionStore::new().with_tool_output(200));
    assert_eq!(session.tool_calls, kept.tool_calls);
    assert_eq!(session.usage, kept.usage);
    assert_eq!(
        call(&session, "toolu_o2").success,
        call(&kept, "toolu_o2").success
    );
}

#[test]
fn a_store_told_to_keeps_an_excerpt_shared_by_the_list_and_the_trace() {
    let session = folded(SessionStore::new().with_tool_output(100));

    // Cut to what the store asked for.
    let long = call(&session, "toolu_o4").output.as_ref().unwrap();
    assert!(long.truncated);
    assert_eq!(long.text.chars().count(), 100);
    assert_eq!(long.lines, 201, "the whole's size is still known");
    let warned = call(&session, "toolu_o3").output.as_ref().unwrap();
    assert!(!warned.text.is_empty() && warned.stderr.is_some());
    assert!(!warned.truncated);
    let ok = call(&session, "toolu_o1").output.as_ref().unwrap();
    assert_eq!(ok.text, "Synthetic output: 3 passed\nall green");
    assert!(!ok.truncated);
    assert!(
        call(&session, "toolu_o7")
            .output
            .as_ref()
            .unwrap()
            .interrupted
    );

    // The same call in the trace holds the same excerpt, not a copy.
    let traced: Vec<&ToolCall> = session.trace[0]
        .thread
        .spans
        .iter()
        .filter_map(|span| match &span.kind {
            SpanKind::Tool(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(traced.len(), 7);
    for in_trace in traced {
        let listed = call(&session, &in_trace.id);
        assert_eq!(listed, in_trace);
        assert!(Arc::ptr_eq(
            listed.output.as_ref().unwrap(),
            in_trace.output.as_ref().unwrap()
        ));
    }

    // A store cannot ask for more than a view holds.
    let most = folded(SessionStore::new().with_tool_output(usize::MAX));
    let long = call(&most, "toolu_o4").output.as_ref().unwrap();
    assert_eq!(long.text.chars().count(), OUTPUT_CHARS);
    assert!(!long.text.contains(MARKER));

    // Reading the same records again changes nothing.
    let mut store = SessionStore::new().with_tool_output(100);
    let records = read(&files().transcript.path);
    store.apply_records(ID, &records);
    let once = store.get(ID).unwrap().clone();
    store.apply_records(ID, &records);
    assert_eq!(store.get(ID).unwrap(), &once);
}

#[test]
fn a_subagents_calls_keep_their_output_too() {
    const WITH_SUBAGENT: &str = "44444444-4444-4444-8444-444444444444";
    let found = discover(&fixture_dir().join("trace")).unwrap();
    let files = found.into_iter().find(|f| f.id == WITH_SUBAGENT).unwrap();
    let mut store = SessionStore::new().with_tool_output(OUTPUT_CHARS);
    store.discovered(&files);
    store.apply_records(WITH_SUBAGENT, &read(&files.transcript.path));
    store.apply_records(WITH_SUBAGENT, &read(&files.subagents[0].transcript.path));
    let session = store.get(WITH_SUBAGENT).unwrap();
    let grep = call(session, "toolu_c1");
    assert!(grep.agent_id.is_some());
    assert_eq!(
        grep.output.as_ref().unwrap().text,
        "Synthetic output of toolu_c1"
    );
    // And the whole is found in the subagent's transcript.
    let whole = read_tool_output(&files, "toolu_c1").unwrap().unwrap();
    assert_eq!(whole.text, "Synthetic output of toolu_c1");
}

#[test]
fn the_whole_output_is_read_again_on_demand() {
    let files = files();
    let get = |id: &str| -> Option<WholeOutput> { read_tool_output(&files, id).unwrap() };

    // What the excerpt cut is all there.
    let long = get("toolu_o4").unwrap();
    assert_eq!(long.text, long_output());
    assert!(long.text.ends_with("SYNTHETIC-TEXT past the limit"));
    assert_eq!((long.truncated, long.file.clone()), (false, None));

    // stderr, an error, a block that is not text, an interruption.
    let warned = get("toolu_o3").unwrap();
    assert_eq!(warned.text, "Synthetic output with a warning");
    assert_eq!(
        warned.stderr.as_deref(),
        Some("synthetic warning: deprecated flag\nsecond line of stderr")
    );
    assert_eq!(
        get("toolu_o2").unwrap().text,
        "Error: synthetic failure, no such file"
    );
    assert_eq!(get("toolu_o1").unwrap().stderr, None);
    let shot = get("toolu_o5").unwrap();
    assert_eq!(
        (shot.text.as_str(), shot.other_blocks),
        ("Synthetic caption of a screenshot", 1)
    );
    let stopped = get("toolu_o7").unwrap();
    assert!(stopped.interrupted && stopped.text.is_empty());

    // An output Claude Code moved out of the transcript is read from its
    // file, not from the preview the transcript has.
    let moved = get("toolu_o6").unwrap();
    let on_disk = files
        .transcript
        .path
        .with_extension("")
        .join("tool-results/toolu_o6.txt");
    assert_eq!(moved.text, fs::read_to_string(&on_disk).unwrap());
    assert_eq!(moved.text.lines().count(), 400);
    assert_eq!(moved.file.as_deref(), Some(on_disk.as_path()));
    assert!(!moved.truncated);

    // A call that is not in the session's files, and ids that are not ids.
    assert_eq!(get("toolu_unknown"), None);
    assert_eq!(get(""), None);
    assert_eq!(get("../../../etc/passwd"), None);
    assert_eq!(get("toolu_o"), None, "a prefix of an id is not the id");
}

#[test]
fn a_call_the_session_no_longer_lists_can_still_be_read() {
    let files = files();
    let mut store = SessionStore::new()
        .with_recent_tools(1)
        .with_tool_output(50);
    store.discovered(&files);
    store.apply_records(ID, &read(&files.transcript.path));
    let before = store.get(ID).unwrap().clone();
    assert_eq!(before.recent_tools.len(), 1);
    assert!(before.recent_tools.iter().all(|call| call.id != "toolu_o4"));

    let whole = read_tool_output(&files, "toolu_o4").unwrap().unwrap();
    assert_eq!(whole.text, long_output());
    // Nothing of it stays in the store.
    assert_eq!(store.get(ID).unwrap(), &before);
}

#[test]
fn a_session_whose_files_went_away_has_no_output() {
    let mut gone = files();
    gone.transcript.path = fixture_dir().join("output/no-such-session.jsonl");
    assert_eq!(read_tool_output(&gone, "toolu_o1").unwrap(), None);
}

#[test]
fn an_unknown_shape_of_tool_use_result_is_surveyed() {
    let record = |result: &str| {
        Record::parse(&format!(
            r#"{{"type":"user","uuid":"u","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":"x"}}]}},"toolUseResult":{result}}}"#
        ))
    };
    let known = [
        record(r#"{"stdout":"x","stderr":""}"#),
        record(r#""Error: x""#),
        record(r#"[{"type":"text","text":"x"}]"#),
    ];
    assert!(Drift::survey(&known).unknown_keys.is_empty());

    let unknown = [
        record("7"),
        record("true"),
        record("null"),
        record(r#"["x"]"#),
    ];
    let drift = Drift::survey(&unknown);
    let names: Vec<&str> = drift.unknown_keys.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "user.toolUseResult: array of non-objects",
            "user.toolUseResult: boolean",
            "user.toolUseResult: null",
            "user.toolUseResult: number",
        ]
    );
    // An unknown shape gives no output beyond the block's own, and breaks
    // nothing.
    let View::User(user) = unknown[0].view() else {
        panic!()
    };
    assert_eq!(user.tool_results[0].output.as_ref().unwrap().text, "x");
}

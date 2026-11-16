//! `discover` against the synthetic projects directory.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use claude_code_sessions::discover::{discover, CwdSource, DiscoverError, SessionFiles};
use claude_code_sessions::record::KNOWN_CLAUDE_CODE_VERSION;

const INDEXED: &str = "11111111-1111-4111-8111-111111111111";
const WITH_SUBAGENTS: &str = "22222222-2222-4222-8222-222222222222";
const UNINDEXED: &str = "33333333-3333-4333-8333-333333333333";

fn projects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
        .join("projects")
}

fn session<'a>(sessions: &'a [SessionFiles], id: &str) -> &'a SessionFiles {
    sessions
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} was not discovered"))
}

#[test]
fn lists_every_session_once() {
    let sessions = discover(&projects_dir()).unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    // Ordered by transcript path. The index also lists a session whose file
    // is gone; only files on disk are sessions.
    assert_eq!(ids, [INDEXED, UNINDEXED, WITH_SUBAGENTS]);

    for found in &sessions {
        assert!(found.transcript.path.is_file());
        assert_eq!(
            found.transcript.path.file_name().unwrap().to_str().unwrap(),
            format!("{}.jsonl", found.id)
        );
        assert_eq!(
            found.transcript.path.parent().unwrap(),
            found.project_dir.as_path()
        );
        assert!(found.transcript.len > 0);
    }
}

#[test]
fn an_indexed_session_takes_its_cwd_from_the_index() {
    let sessions = discover(&projects_dir()).unwrap();
    let found = session(&sessions, INDEXED);
    // Its records say `/work/example/crates/sub`; the index wins.
    assert_eq!(found.cwd.as_deref(), Some(Path::new("/work/example")));
    assert_eq!(found.cwd_source, Some(CwdSource::Index));
    assert!(found.project_dir.ends_with("-work-example"));
    assert!(found.subagents.is_empty());
}

#[test]
fn an_unindexed_session_takes_its_cwd_from_its_transcript() {
    let sessions = discover(&projects_dir()).unwrap();

    // Same project as the indexed session, but the index does not list it.
    let found = session(&sessions, UNINDEXED);
    assert_eq!(found.cwd.as_deref(), Some(Path::new("/work/example")));
    assert_eq!(found.cwd_source, Some(CwdSource::Transcript));

    // No index at all, and the first record (`mode`) carries no cwd. The
    // directory name `-work-other-repo` could not have told `.` from `/`.
    let found = session(&sessions, WITH_SUBAGENTS);
    assert_eq!(found.cwd.as_deref(), Some(Path::new("/work/other.repo")));
    assert_eq!(found.cwd_source, Some(CwdSource::Transcript));
    assert!(found.project_dir.ends_with("-work-other-repo"));
}

#[test]
fn subagents_attach_to_their_session() {
    let sessions = discover(&projects_dir()).unwrap();
    let found = session(&sessions, WITH_SUBAGENTS);
    let ids: Vec<&str> = found
        .subagents
        .iter()
        .map(|s| s.agent_id.as_str())
        .collect();
    assert_eq!(ids, ["a1b2c3d4e5f60718", "b2c3d4e5f6071829"]);

    let with_meta = &found.subagents[0];
    assert!(with_meta.transcript.path.ends_with(format!(
        "{WITH_SUBAGENTS}/subagents/agent-a1b2c3d4e5f60718.jsonl"
    )));
    assert!(with_meta.transcript.len > 0);
    let meta = with_meta.meta.as_ref().unwrap();
    assert_eq!(meta.agent_type.as_deref(), Some("Explore"));
    assert_eq!(meta.description.as_deref(), Some("Synthetic agent task"));
    assert_eq!(meta.tool_use_id.as_deref(), Some("toolu_02"));
    assert_eq!(meta.spawn_depth, Some(1));
    assert_eq!(meta.finished, Some(true));

    // A subagent whose metadata file is missing is still a subagent.
    assert_eq!(found.subagents[1].meta, None);
}

#[test]
fn modification_times_answer_recency_without_reading() {
    let sessions = discover(&projects_dir()).unwrap();
    let now = SystemTime::now();
    for found in &sessions {
        let modified = found.transcript.modified.expect("a modification time");
        assert!(modified <= now + Duration::from_secs(60));
        assert!(found.transcript.modified_since(SystemTime::UNIX_EPOCH));
        assert!(!found
            .transcript
            .modified_since(now + Duration::from_secs(3600)));
    }
}

#[test]
fn a_missing_projects_directory_is_a_typed_error() {
    let missing = projects_dir().join("no-such-directory");
    match discover(&missing) {
        Err(DiscoverError::Missing(path)) => assert_eq!(path, missing),
        other => panic!("expected Missing, got {other:?}"),
    }
}

#[test]
fn a_file_where_the_directory_should_be_is_a_typed_error() {
    let file = projects_dir().join("stray-file.txt");
    match discover(&file) {
        Err(DiscoverError::Unreadable { path, .. }) => assert_eq!(path, file),
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

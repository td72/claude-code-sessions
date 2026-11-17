//! The loop that keeps a store current, and what is derived from a session.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iktinos_core::follow::Follower;
use iktinos_core::record::{Timestamp, KNOWN_CLAUDE_CODE_VERSION};
use iktinos_core::session::{SessionStore, StatusKind};

const INDEXED: &str = "11111111-1111-4111-8111-111111111111";
const WITH_SUBAGENTS: &str = "22222222-2222-4222-8222-222222222222";

fn fixture_projects() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
        .join("projects")
}

/// A copy of the fixture projects that a test may write to.
fn scratch(name: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("iktinos-follow-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy(&fixture_projects(), &dir);
    dir
}

fn transcript(projects: &Path) -> PathBuf {
    projects
        .join("-work-other-repo")
        .join(format!("{WITH_SUBAGENTS}.jsonl"))
}

/// One more record of the session, later than every other: a prompt.
fn prompt(minute: u8) -> String {
    format!(
        r#"{{"parentUuid":null,"isSidechain":false,"promptId":"prompt-f{minute}","type":"user","message":{{"role":"user","content":"Synthetic prompt: one more"}},"uuid":"f{minute}","timestamp":"2026-11-02T11:{minute:02}:00.000Z","cwd":"/work/other.repo","sessionId":"{WITH_SUBAGENTS}","version":"2.1.325","gitBranch":"main"}}"#
    )
}

#[test]
fn the_first_poll_reads_everything_and_the_next_only_what_is_new() {
    let projects = scratch("appends");
    let mut store = SessionStore::new();
    let mut follower = Follower::new(&projects);
    assert_eq!(follower.projects_dir(), projects);
    assert!(
        follower.files(WITH_SUBAGENTS).is_none(),
        "nothing is read yet"
    );

    let first = follower.poll(&mut store).unwrap();
    assert_eq!(store.len(), 3);
    assert!(first.records > 0);
    assert!(first.restarted.is_empty());
    // The session's files are known, its subagents' among them.
    let files = follower.files(WITH_SUBAGENTS).unwrap();
    assert_eq!(files.transcript.path, transcript(&projects));
    assert_eq!(files.subagents.len(), 2);
    assert_eq!(store.get(WITH_SUBAGENTS).unwrap().subagents.len(), 2);
    let interactions = store.get(WITH_SUBAGENTS).unwrap().trace.len();

    // Nothing was appended: nothing is read.
    let idle = follower.poll(&mut store).unwrap();
    assert_eq!((idle.records, idle.restarted.len()), (0, 0));

    // One record more: that one is read.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(transcript(&projects))
        .unwrap();
    writeln!(file, "{}", prompt(5)).unwrap();
    let appended = follower.poll(&mut store).unwrap();
    assert_eq!(appended.records, 1);
    assert_eq!(
        store.get(WITH_SUBAGENTS).unwrap().trace.len(),
        interactions + 1
    );

    let _ = fs::remove_dir_all(projects);
}

#[test]
fn a_replaced_transcript_makes_its_session_start_over() {
    let projects = scratch("replaced");
    let mut store = SessionStore::new();
    let mut follower = Follower::new(&projects);
    follower.poll(&mut store).unwrap();
    let before = store.get(WITH_SUBAGENTS).unwrap().clone();
    assert!(before.tool_calls > 0);

    // The file is written anew, shorter: one prompt and nothing else.
    fs::write(transcript(&projects), format!("{}\n", prompt(7))).unwrap();
    let followed = follower.poll(&mut store).unwrap();
    assert_eq!(followed.restarted, [WITH_SUBAGENTS]);
    let after = store.get(WITH_SUBAGENTS).unwrap();
    // What the old file said is forgotten; the subagents' files were read
    // again and still count.
    assert_eq!(after.trace.len(), 1);
    assert!(after.tool_calls < before.tool_calls);
    assert_eq!(after.subagents.len(), 2);
    // The other sessions are as they were.
    assert_eq!(store.len(), 3);

    let _ = fs::remove_dir_all(projects);
}

#[test]
fn a_directory_that_cannot_be_listed_is_an_error_and_what_is_known_is_still_read() {
    let mut store = SessionStore::new();
    let mut missing = Follower::new("/no-such-root/projects");
    assert!(missing.poll(&mut store).is_err());
    assert_eq!(store.len(), 0);

    // A directory that goes away after it was read.
    let projects = scratch("gone");
    let mut follower = Follower::new(&projects);
    follower.poll(&mut store).unwrap();
    let moved = projects.with_extension("moved");
    let _ = fs::remove_dir_all(&moved);
    fs::rename(&projects, &moved).unwrap();
    assert!(follower.poll(&mut store).is_err());
    assert_eq!(store.len(), 3, "what was read is kept");
    // And back: it is followed again.
    fs::rename(&moved, &projects).unwrap();
    assert!(follower.poll(&mut store).is_ok());

    let _ = fs::remove_dir_all(projects);
}

#[test]
fn a_session_says_what_it_is_called_where_it_runs_and_what_it_does() {
    let mut store = SessionStore::new();
    Follower::new(fixture_projects()).poll(&mut store).unwrap();

    let titled = store.get(INDEXED).unwrap();
    assert_eq!(titled.label(), "Synthetic session title");
    assert_eq!(titled.location(), Some(Path::new("/work/example")));
    assert_eq!(titled.location_name().as_deref(), Some("example"));

    // Without a title or a last prompt, the start of its id.
    let untitled = store.get(WITH_SUBAGENTS).unwrap();
    assert_eq!(untitled.label(), "22222222");
    assert_eq!(untitled.location_name().as_deref(), Some("other.repo"));
    assert_eq!(untitled.last_seen(), untitled.last_activity);

    // From the transcript alone: active while its last record is recent,
    // idle after.
    let last = untitled.last_activity.unwrap();
    let minute = Duration::from_secs(60);
    let soon = Timestamp::from_millis(last.as_millis() + 5_000);
    let later = Timestamp::from_millis(last.as_millis() + 3_600_000);
    let status = untitled.status(soon, minute);
    assert_eq!(
        (status.kind, status.text.as_str()),
        (StatusKind::Busy, "active")
    );
    assert_eq!(untitled.status(later, minute).kind, StatusKind::Idle);
    // How recent is recent is the caller's to say.
    assert_eq!(
        untitled.status(soon, Duration::from_secs(1)).kind,
        StatusKind::Idle
    );
}

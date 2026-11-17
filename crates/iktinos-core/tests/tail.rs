//! `tail` against files written during the test.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use iktinos_core::record::{View, KNOWN_CLAUDE_CODE_VERSION};
use iktinos_core::tail::Tail;

/// A fresh directory under the target directory, one per call.
fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "tail-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
        .join(name);
    fs::read_to_string(path).unwrap()
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// A synthetic record of about `width` bytes carrying its own number.
fn numbered(n: usize, width: usize) -> String {
    let line = format!(r#"{{"type":"mode","mode":"normal","n":{n},"pad":""#);
    let pad = width.saturating_sub(line.len() + 3);
    format!("{line}{}\"}}\n", "x".repeat(pad))
}

fn number(line: &[u8]) -> u64 {
    let value: serde_json::Value = serde_json::from_slice(line).unwrap();
    value["n"].as_u64().unwrap()
}

#[test]
fn an_append_is_picked_up_by_the_next_poll() {
    let path = scratch("append").join("session.jsonl");
    let first = fixture("plain-turn.jsonl");
    append(&path, &first);

    let mut tail = Tail::from_start(&path);
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), first.lines().count());
    assert!(!batch.restarted);
    assert_eq!(batch.bytes_read, first.len() as u64);
    assert_eq!(tail.offset(), first.len() as u64);
    assert_eq!(batch.lines.last().unwrap().end_offset, first.len() as u64);
    // Each line ends where the next one starts.
    let mut expected_end = 0;
    for (line, text) in batch.lines.iter().zip(first.lines()) {
        expected_end += text.len() as u64 + 1;
        assert_eq!(line.end_offset, expected_end);
        assert_eq!(line.bytes, text.as_bytes());
    }
    assert!(matches!(batch.lines[1].record().view(), View::User(_)));

    // Nothing new: nothing read.
    let batch = tail.poll().unwrap();
    assert!(batch.lines.is_empty());
    assert_eq!(batch.bytes_read, 0);

    let second = fixture("tool-call.jsonl");
    append(&path, &second);
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), second.lines().count());
    assert_eq!(batch.bytes_read, second.len() as u64);
    assert_eq!(tail.offset(), (first.len() + second.len()) as u64);
    assert!(matches!(
        batch.lines[2].record().view(),
        View::Assistant(a) if a.tool_uses.len() == 1
    ));
}

#[test]
fn a_half_written_line_is_held_back_until_it_is_complete() {
    let path = scratch("partial").join("session.jsonl");
    let whole = numbered(1, 80);
    let second = numbered(2, 80);
    let (head, rest) = second.split_at(30);
    append(&path, &whole);
    append(&path, head);

    let mut tail = Tail::from_start(&path);
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(number(&batch.lines[0].bytes), 1);
    assert_eq!(tail.offset(), whole.len() as u64);

    // Still partial: still held back, and the offset has not moved.
    assert!(tail.poll().unwrap().lines.is_empty());
    assert_eq!(tail.offset(), whole.len() as u64);

    append(&path, rest);
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(number(&batch.lines[0].bytes), 2);
    assert_eq!(tail.offset(), (whole.len() * 2) as u64);
}

#[test]
fn blank_lines_are_skipped_and_line_endings_trimmed() {
    let path = scratch("blank").join("session.jsonl");
    let (one, two) = (numbered(1, 60), numbered(2, 60));
    let text = format!("{}\r\n\n   \n{two}", one.trim_end());
    append(&path, &text);

    let mut tail = Tail::from_start(&path);
    let batch = tail.poll().unwrap();
    let numbers: Vec<u64> = batch.lines.iter().map(|l| number(&l.bytes)).collect();
    assert_eq!(numbers, [1, 2]);
    assert_eq!(batch.lines[0].bytes, one.trim_end().as_bytes());
    assert_eq!(tail.offset(), text.len() as u64);
}

#[test]
fn a_truncated_file_is_read_again_from_the_beginning() {
    let path = scratch("truncate").join("session.jsonl");
    append(&path, &(1..=5).map(|n| numbered(n, 80)).collect::<String>());
    let mut tail = Tail::from_start(&path);
    assert_eq!(tail.poll().unwrap().lines.len(), 5);

    fs::write(&path, numbered(9, 80)).unwrap();
    let batch = tail.poll().unwrap();
    assert!(batch.restarted);
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(number(&batch.lines[0].bytes), 9);

    // A restart is reported once.
    assert!(!tail.poll().unwrap().restarted);
}

#[cfg(unix)]
#[test]
fn a_replaced_file_is_read_again_even_when_it_is_longer() {
    let dir = scratch("replace");
    let path = dir.join("session.jsonl");
    append(&path, &numbered(1, 80));
    let mut tail = Tail::from_start(&path);
    assert_eq!(tail.poll().unwrap().lines.len(), 1);

    // A new file (a new inode) moved over the old one, longer than what was
    // read: size alone would mistake it for an append.
    let replacement = dir.join("replacement.jsonl");
    append(
        &replacement,
        &(7..=9).map(|n| numbered(n, 80)).collect::<String>(),
    );
    fs::rename(&replacement, &path).unwrap();

    let batch = tail.poll().unwrap();
    assert!(batch.restarted);
    let numbers: Vec<u64> = batch.lines.iter().map(|l| number(&l.bytes)).collect();
    assert_eq!(numbers, [7, 8, 9]);
}

#[test]
fn the_first_look_at_a_large_file_reads_a_bounded_number_of_bytes() {
    let path = scratch("large").join("session.jsonl");
    const LINES: usize = 20_000;
    const WIDTH: usize = 256;
    const WINDOW: u64 = 64 * 1024;
    let text: String = (0..LINES).map(|n| numbered(n, WIDTH)).collect();
    fs::write(&path, &text).unwrap();
    assert!(text.len() > 5_000_000);

    let mut tail = Tail::from_end(&path, WINDOW);
    let batch = tail.poll().unwrap();
    assert!(batch.bytes_read <= WINDOW + 1, "{}", batch.bytes_read);
    assert!(!batch.restarted);

    // Whole lines only, consecutive, up to the last one in the file.
    let numbers: Vec<u64> = batch.lines.iter().map(|l| number(&l.bytes)).collect();
    assert!(numbers.len() as u64 >= WINDOW / WIDTH as u64 - 1);
    assert_eq!(*numbers.last().unwrap(), LINES as u64 - 1);
    assert!(numbers.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert_eq!(tail.offset(), text.len() as u64);

    // Afterwards it follows appends like any other tail.
    append(&path, &numbered(LINES, WIDTH));
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(number(&batch.lines[0].bytes), LINES as u64);
    assert_eq!(batch.bytes_read, WIDTH as u64);
}

#[test]
fn a_window_that_starts_on_a_line_boundary_keeps_that_line() {
    let path = scratch("boundary").join("session.jsonl");
    let text: String = (0..10).map(|n| numbered(n, 100)).collect();
    fs::write(&path, &text).unwrap();

    // Exactly the last three lines.
    let mut tail = Tail::from_end(&path, 300);
    let numbers: Vec<u64> = tail
        .poll()
        .unwrap()
        .lines
        .iter()
        .map(|l| number(&l.bytes))
        .collect();
    assert_eq!(numbers, [7, 8, 9]);

    // One byte less cuts into line 7, which is then left out.
    let mut tail = Tail::from_end(&path, 299);
    assert_eq!(tail.poll().unwrap().lines.len(), 2);
}

#[test]
fn a_window_larger_than_the_file_reads_all_of_it() {
    let path = scratch("small").join("session.jsonl");
    let text: String = (0..3).map(|n| numbered(n, 100)).collect();
    fs::write(&path, &text).unwrap();
    for window in [text.len() as u64, 1 << 20] {
        let mut tail = Tail::from_end(&path, window);
        assert_eq!(tail.poll().unwrap().lines.len(), 3);
    }
}

#[test]
fn a_line_longer_than_the_window_is_skipped() {
    let path = scratch("long-line").join("session.jsonl");
    let text = format!("{}{}", numbered(0, 100), numbered(1, 10_000));
    fs::write(&path, &text).unwrap();

    let mut tail = Tail::from_end(&path, 1_000);
    let batch = tail.poll().unwrap();
    assert!(batch.lines.is_empty());
    assert!(batch.bytes_read <= 1_001);
    assert_eq!(tail.offset(), text.len() as u64);

    append(&path, &numbered(2, 100));
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(number(&batch.lines[0].bytes), 2);
}

#[test]
fn a_saved_offset_resumes_without_reading_again() {
    let path = scratch("resume").join("session.jsonl");
    append(&path, &(0..4).map(|n| numbered(n, 100)).collect::<String>());
    let mut first = Tail::from_start(&path);
    let saved = first.poll().unwrap().lines[1].end_offset;

    let mut resumed = Tail::at(&path, saved);
    let batch = resumed.poll().unwrap();
    let numbers: Vec<u64> = batch.lines.iter().map(|l| number(&l.bytes)).collect();
    assert_eq!(numbers, [2, 3]);
    assert_eq!(batch.bytes_read, 200);
    assert!(!batch.restarted);
}

#[test]
fn poll_each_streams_the_same_lines() {
    let path = scratch("each").join("session.jsonl");
    let text: String = (0..50).map(|n| numbered(n, 100)).collect();
    fs::write(&path, &text).unwrap();

    let mut seen = Vec::new();
    let mut tail = Tail::from_start(&path);
    let polled = tail
        .poll_each(|line, end| seen.push((number(line), end)))
        .unwrap();
    assert_eq!(polled.lines, 50);
    assert_eq!(polled.bytes_read, text.len() as u64);
    assert_eq!(seen.len(), 50);
    assert_eq!(seen[49], (49, text.len() as u64));
}

#[test]
fn a_missing_file_is_an_error_and_the_offset_survives_it() {
    let path = scratch("missing").join("session.jsonl");
    let mut tail = Tail::from_start(&path);
    assert_eq!(tail.poll().unwrap_err().kind(), ErrorKind::NotFound);

    append(&path, &numbered(0, 100));
    assert_eq!(tail.poll().unwrap().lines.len(), 1);
}

#[test]
fn debug_of_a_line_leaves_its_content_out() {
    let path = scratch("debug").join("session.jsonl");
    append(&path, "{\"type\":\"user\",\"text\":\"SYNTHETIC-TEXT\"}\n");
    let batch = Tail::from_start(&path).poll().unwrap();
    assert!(!format!("{batch:?}").contains("SYNTHETIC-TEXT"));
}

#[cfg(feature = "watch")]
#[test]
fn the_watcher_reports_a_transcript_that_was_appended_to() {
    use iktinos_core::tail::watch::Watcher;
    use std::time::{Duration, Instant};

    let dir = scratch("watch");
    let project = dir.join("-work-example");
    fs::create_dir_all(&project).unwrap();
    let path = project.join("session.jsonl");
    append(&path, &numbered(0, 100));
    append(&project.join("notes.txt"), "not a transcript\n");

    let mut tail = Tail::from_start(&path);
    assert_eq!(tail.poll().unwrap().lines.len(), 1);

    let (_watcher, changed) = Watcher::new(&dir).unwrap();
    // Give the backend a moment to start before the write it should see.
    std::thread::sleep(Duration::from_millis(300));
    append(&project.join("notes.txt"), "still not a transcript\n");
    append(&path, &numbered(1, 100));

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let reported = changed
            .recv_timeout(left)
            .expect("the append was not reported in time");
        // Only transcripts are reported. Compare by name: the backend may
        // report a canonicalized path.
        assert_eq!(reported.extension().unwrap(), "jsonl");
        if reported.file_name() == path.file_name() {
            break;
        }
    }
    let batch = tail.poll().unwrap();
    assert_eq!(batch.lines.len(), 1);
    assert_eq!(number(&batch.lines[0].bytes), 1);
}

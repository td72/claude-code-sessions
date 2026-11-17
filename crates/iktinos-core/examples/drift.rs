//! Surveys a projects directory for transcript shapes the crate does not know.
//!
//! ```text
//! cargo run -p iktinos-core --example drift [PROJECTS_DIR]
//! ```
//!
//! `PROJECTS_DIR` defaults to `~/.claude/projects`. The report holds record
//! types, key names and counts only; no value of any record is printed.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use iktinos_core::record::drift::Drift;
use iktinos_core::record::Record;

fn main() -> ExitCode {
    let dir = match env::args_os().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => match env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".claude/projects"),
            None => {
                eprintln!("usage: drift PROJECTS_DIR");
                return ExitCode::FAILURE;
            }
        },
    };

    let mut files = Vec::new();
    if let Err(e) = transcripts(&dir, &mut files) {
        eprintln!("{}: {e}", dir.display());
        return ExitCode::FAILURE;
    }

    let mut drift = Drift::new();
    let mut unreadable = 0;
    for file in &files {
        if survey(file, &mut drift).is_err() {
            unreadable += 1;
        }
    }

    println!("files: {} ({unreadable} unreadable)", files.len());
    print!("{drift}");
    ExitCode::SUCCESS
}

/// Every `*.jsonl` below `dir`, subagent transcripts included.
fn transcripts(dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            // A directory that vanishes or cannot be read is not the survey's
            // problem.
            let _ = transcripts(&path, found);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            found.push(path);
        }
    }
    Ok(())
}

fn survey(file: &Path, drift: &mut Drift) -> io::Result<()> {
    let mut reader = BufReader::new(fs::File::open(file)?);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let trimmed = line.trim_ascii();
        if !trimmed.is_empty() {
            drift.observe(&Record::parse_bytes(trimmed));
        }
    }
}

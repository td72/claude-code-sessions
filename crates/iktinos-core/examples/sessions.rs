//! Folds every transcript under a projects directory and prints one line per
//! session: numbers only, no titles or prompts.
//!
//! ```text
//! cargo run --release -p iktinos-core --example sessions [PROJECTS_DIR]
//! ```

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iktinos_core::discover::{default_projects_dir, discover};
use iktinos_core::record::{Record, Timestamp};
use iktinos_core::session::{Activity, CostSource, SessionStore};
use iktinos_core::tail::Tail;

fn main() -> ExitCode {
    let Some(dir) = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(default_projects_dir)
    else {
        eprintln!("usage: sessions PROJECTS_DIR");
        return ExitCode::FAILURE;
    };
    let found = match discover(&dir) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    let mut store = SessionStore::new();
    let (mut records, mut bytes) = (0u64, 0u64);
    let mut fold = |store: &mut SessionStore, id: &str, path: &Path| {
        let polled = Tail::from_start(path).poll_each(|line, _| {
            store.apply(id, &Record::parse_bytes(line));
            records += 1;
        });
        if let Ok(polled) = polled {
            bytes += polled.bytes_read;
        }
    };
    for files in &found {
        store.discovered(files);
        fold(&mut store, &files.id, &files.transcript.path);
        for subagent in &files.subagents {
            fold(&mut store, &files.id, &subagent.transcript.path);
        }
    }
    let elapsed = started.elapsed();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let now = Timestamp::from_millis(now);
    println!(
        "{:8}  {:7}  {:18}  {:>5}  {:>5}  {:>4}  {:>12}  {:>7}  {:>9}  source",
        "session", "state", "model", "turns", "tools", "subs", "tokens", "context", "cost"
    );
    for session in store.sessions() {
        let state = match session.activity(now, Duration::from_secs(60)) {
            Activity::Active { .. } => "active",
            Activity::Idle => "idle",
            Activity::Exited => "exited",
        };
        let context = session
            .context
            .and_then(|c| c.fill())
            .map_or("—".to_owned(), |f| format!("{:.0}%", f * 100.0));
        let cost = session
            .cost
            .usd
            .map_or("—".to_owned(), |usd| format!("${usd:.2}"));
        let source = match session.cost.source {
            CostSource::Unknown => "",
            CostSource::Reported => "reported",
            CostSource::ReportedPlusEstimate => "reported+estimate",
            CostSource::Estimated => "estimate",
        };
        println!(
            "{:8.8}  {:7}  {:18.18}  {:>5}  {:>5}  {:>4}  {:>12}  {:>7}  {:>9}  {}{}",
            session.id,
            state,
            session.model.as_deref().unwrap_or("—"),
            session.turns,
            session.tool_calls,
            session.subagents.len(),
            session.usage.total().total_tokens(),
            context,
            cost,
            source,
            if session.cost.incomplete {
                " (partial)"
            } else {
                ""
            },
        );
    }
    println!(
        "{} sessions, {records} records, {:.1} MB in {:.2?}",
        store.len(),
        bytes as f64 / 1e6,
        elapsed
    );
    ExitCode::SUCCESS
}

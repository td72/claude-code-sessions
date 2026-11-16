//! `iktinos`: a small terminal viewer for the Claude Code sessions
//! on this machine. A session list and a detail pane over
//! `iktinos::session::SessionStore`, with the live layer when
//! the bridge mod is installed. Mainly a development and standalone tool; vig
//! renders the same model as a full page.
//!
//! Read-only: it reads the transcripts and listens on its own socket.

mod app;
mod format;
mod ui;

use std::env;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iktinos::discover::default_projects_dir;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::{App, Config};

/// How often the transcripts are read again.
const REFRESH_EVERY: Duration = Duration::from_secs(1);

const USAGE: &str = "\
A small viewer for the Claude Code sessions on this machine.

Usage: iktinos [OPTIONS]

Options:
      --projects-dir <DIR>  Read transcripts from DIR instead of ~/.claude/projects
      --socket-dir <DIR>    Listen for live events in DIR instead of the shared default
      --no-live             Do not listen for live events from the bridge mod
  -h, --help                Print help
  -V, --version             Print version
";

fn main() -> ExitCode {
    let config = match parse_args(env::args().skip(1)) {
        Ok(Parsed::Run(config)) => config,
        Ok(Parsed::Print(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("iktinos: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let mut app = App::new(config);
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("iktinos: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut refreshed = Instant::now();
    loop {
        let now = app::now();
        terminal.draw(|frame| ui::draw(frame, app, now))?;

        let wait = REFRESH_EVERY.saturating_sub(refreshed.elapsed());
        if event::poll(wait)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                    KeyCode::Up | KeyCode::Char('k') => app.select_previous(),
                    KeyCode::Home | KeyCode::Char('g') => app.select_first(),
                    KeyCode::End | KeyCode::Char('G') => app.select_last(),
                    KeyCode::Char('r') => {
                        app.refresh();
                        refreshed = Instant::now();
                    }
                    _ => {}
                }
            }
        }
        if refreshed.elapsed() >= REFRESH_EVERY {
            app.refresh();
            refreshed = Instant::now();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Config),
    Print(String),
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut projects_dir = None;
    let mut socket_dir = None;
    let mut live = true;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg, None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a directory"))
        };
        match flag.as_str() {
            "--projects-dir" => projects_dir = Some(value("--projects-dir")?),
            "--socket-dir" => socket_dir = Some(value("--socket-dir")?),
            "--no-live" => live = false,
            "-h" | "--help" => return Ok(Parsed::Print(USAGE.to_owned())),
            "-V" | "--version" => {
                return Ok(Parsed::Print(format!(
                    "iktinos {}\n",
                    env!("CARGO_PKG_VERSION")
                )))
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    let projects_dir = projects_dir
        .or_else(default_projects_dir)
        .ok_or("no home directory; pass --projects-dir")?;
    Ok(Parsed::Run(Config {
        projects_dir,
        live,
        socket_dir,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn options_are_read_in_both_spellings() {
        let expected = Parsed::Run(Config {
            projects_dir: PathBuf::from("/synthetic/projects"),
            live: false,
            socket_dir: Some(PathBuf::from("/synthetic/sockets")),
        });
        assert_eq!(
            parse(&[
                "--projects-dir",
                "/synthetic/projects",
                "--socket-dir=/synthetic/sockets",
                "--no-live"
            ]),
            Ok(expected)
        );
        let Ok(Parsed::Run(config)) = parse(&["--projects-dir=/p"]) else {
            panic!()
        };
        assert!(config.live);
        assert_eq!(config.socket_dir, None);
    }

    #[test]
    fn help_version_and_mistakes() {
        assert_eq!(parse(&["--help"]), Ok(Parsed::Print(USAGE.to_owned())));
        assert!(matches!(parse(&["-V"]), Ok(Parsed::Print(v)) if v.starts_with("iktinos ")));
        assert!(parse(&["--projects-dir"])
            .unwrap_err()
            .contains("needs a directory"));
        assert!(parse(&["--projects-dir="]).is_err());
        assert!(parse(&["--frobnicate"])
            .unwrap_err()
            .contains("--frobnicate"));
        assert!(parse(&["stray"]).is_err());
    }
}

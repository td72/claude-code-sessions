//! `iktinos`: a terminal viewer for the Claude Code sessions on this machine.
//! Sessions, recent tool calls, subagents, the trace of the latest interactions and
//! usage, a page each, over
//! `iktinos::session::SessionStore`, with the live layer when the bridge mod
//! is installed. Its pages and keys follow vig, which renders the same model
//! as a page of its own.
//!
//! Read-only: it reads the transcripts and its configuration file, and
//! listens on its own socket.

mod app;
mod config;
mod format;
mod highlight;
mod ui;

use std::env;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use iktinos::discover::default_projects_dir;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use app::{App, Config};
use config::{Args, Env};

const USAGE: &str = "\
A small viewer for the Claude Code sessions on this machine.

Usage: iktinos [OPTIONS]

Options:
      --projects-dir <DIR>  Read transcripts from DIR instead of ~/.claude/projects
      --socket-dir <DIR>    Listen for live events in DIR instead of the shared default
      --no-live             Do not listen for live events from the bridge mod
      --config <FILE>       Read the configuration from FILE instead of
                            ~/.config/iktinos/config.toml; an argument goes
                            before the file, the file before the defaults
      --cwd                 Show only the sessions that run in the current directory,
                            below it, or in a worktree of its git repository
                            (the Usage page still totals every session)
      --time <STYLE>        Show times as `clock` (the local date and time, the
                            default) or `relative` (how long ago); T switches
      --ago <DURATION>      With `clock`, still show what is newer than DURATION
                            as how long ago: 90s, 30m, 1h (the default), 2d;
                            0 for never
  -h, --help                Print help
  -V, --version             Print version
";

fn main() -> ExitCode {
    let args = match parse_args(env::args().skip(1)) {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Print(text)) => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("iktinos: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // What is wrong with a configuration file is said as it is: the usage
    // would not help.
    let config = match configure(args) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("iktinos: {message}");
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
    // The first `g` of `gg`.
    let mut pending_g = false;
    loop {
        let now = app::now();
        terminal.draw(|frame| ui::draw(frame, app, now))?;

        let wait = app.config.refresh.saturating_sub(refreshed.elapsed());
        if event::poll(wait)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    let was_g = std::mem::take(&mut pending_g);
                    match handle_key(app, key, was_g) {
                        Handled::Quit => return Ok(()),
                        Handled::Refresh => {
                            app.refresh();
                            refreshed = Instant::now();
                        }
                        Handled::PendingG => pending_g = true,
                        Handled::Done => {}
                    }
                }
            }
        }
        if refreshed.elapsed() >= app.config.refresh {
            app.refresh();
            refreshed = Instant::now();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handled {
    Done,
    Quit,
    Refresh,
    /// A `g` that waits for a second one.
    PendingG,
}

/// Applies one key. `after_g` says the previous key was a lone `g`.
fn handle_key(app: &mut App, key: KeyEvent, after_g: bool) -> Handled {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        return Handled::Quit;
    }
    // Any key closes the help.
    if app.help {
        app.help = false;
        return Handled::Done;
    }
    // Typing a search takes every key until Enter or Esc.
    if let Some(typed) = &mut app.typing {
        match key.code {
            KeyCode::Enter => app.submit_search(),
            KeyCode::Esc => app.typing = None,
            KeyCode::Backspace => {
                typed.pop();
            }
            KeyCode::Char(c) if !ctrl => typed.push(c),
            _ => {}
        }
        return Handled::Done;
    }

    match key.code {
        KeyCode::Char('q') => return Handled::Quit,
        KeyCode::Char('r') => return Handled::Refresh,
        KeyCode::Char('?') => app.help = true,
        // A digit past the pages that are shown does nothing.
        KeyCode::Char(c @ '1'..='9') => {
            if let Some(page) = app.page_at(c as usize - '1' as usize) {
                app.go_to(page);
            }
        }
        KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
        KeyCode::Char('i') => app.focus_detail(),
        KeyCode::Char('l') | KeyCode::Right => app.right(),
        KeyCode::Enter => app.enter(),
        KeyCode::Char('h') | KeyCode::Left => app.leave_detail(),
        KeyCode::Esc => app.back(),
        KeyCode::Char('d') if ctrl => app.move_by(app.half_page()),
        KeyCode::Char('u') if ctrl => app.move_by(-app.half_page()),
        KeyCode::Char('j') | KeyCode::Down => app.move_by(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_by(-1),
        KeyCode::Char('g') if after_g => app.move_first(),
        KeyCode::Char('g') => return Handled::PendingG,
        KeyCode::Home => app.move_first(),
        KeyCode::Char('G') | KeyCode::End => app.move_last(),
        KeyCode::Char('/') => app.start_search(),
        KeyCode::Char('n') => app.next_match(1),
        KeyCode::Char('N') => app.next_match(-1),
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('t') => app.toggle_by_session(),
        KeyCode::Char('T') => app.toggle_time(),
        KeyCode::Char('o') => app.load_output(),
        KeyCode::Char('f') => app.cycle_filter(),
        KeyCode::Char('F') => app.toggle_repository(),
        _ => {}
    }
    Handled::Done
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Args),
    Print(String),
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut asked = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg, None),
        };
        let mut value = |name: &str, what: &str| {
            inline
                .clone()
                .or_else(|| args.next())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a {what}"))
        };
        match flag.as_str() {
            "--config" => asked.config = Some(value("--config", "file")?),
            "--projects-dir" => asked.projects_dir = Some(value("--projects-dir", "directory")?),
            "--socket-dir" => asked.socket_dir = Some(value("--socket-dir", "directory")?),
            "--no-live" => asked.no_live = true,
            "--cwd" => {
                if inline.is_some() {
                    return Err("--cwd takes no value".to_owned());
                }
                asked.cwd = true;
            }
            "--ago" => {
                let length = inline.clone().or_else(|| args.next()).unwrap_or_default();
                asked.ago = Some(app::parse_duration(&length).ok_or_else(|| {
                    format!("--ago is a length of time such as 30m or 1h, not '{length}'")
                })?);
            }
            "--time" => {
                let style = inline.clone().or_else(|| args.next()).unwrap_or_default();
                asked.time =
                    Some(app::TimeStyle::parse(&style).ok_or_else(|| {
                        format!("--time is `clock` or `relative`, not '{style}'")
                    })?);
            }
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
    Ok(Parsed::Run(asked))
}

/// The settings to run with: the arguments, then the configuration file,
/// then the defaults.
fn configure(args: Args) -> Result<Config, String> {
    let home = env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let file = match &args.config {
        Some(path) => config::load(path, true)?,
        None => match config::default_path(env::var_os("XDG_CONFIG_HOME"), home.as_deref()) {
            Some(path) => config::load(&path, false)?,
            None => config::File::default(),
        },
    };
    let env = Env {
        home,
        current_dir: env::current_dir().ok(),
        default_projects_dir: default_projects_dir(),
    };
    config::resolve(args, file, &env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Page;
    use std::time::Duration;

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn options_are_read_in_both_spellings() {
        let expected = Args {
            projects_dir: Some(PathBuf::from("/synthetic/projects")),
            socket_dir: Some(PathBuf::from("/synthetic/sockets")),
            no_live: true,
            config: Some(PathBuf::from("/synthetic/config.toml")),
            ..Args::default()
        };
        assert_eq!(
            parse(&[
                "--projects-dir",
                "/synthetic/projects",
                "--socket-dir=/synthetic/sockets",
                "--no-live",
                "--config",
                "/synthetic/config.toml"
            ]),
            Ok(Parsed::Run(expected))
        );
        // What is not asked for is left to the configuration file.
        assert_eq!(parse(&[]), Ok(Parsed::Run(Args::default())));
        assert_eq!(
            parse(&["--config"]),
            Err("--config needs a file".to_owned())
        );
        assert_eq!(
            parse(&["--projects-dir="]),
            Err("--projects-dir needs a directory".to_owned())
        );

        let asked = |args: &[&str]| match parse(args) {
            Ok(Parsed::Run(args)) => args,
            other => panic!("{other:?}"),
        };
        assert!(asked(&["--cwd"]).cwd);
        // A flag: the directory is where the viewer was started, not named.
        assert_eq!(
            parse(&["--cwd=/work/example"]),
            Err("--cwd takes no value".to_owned())
        );
        assert_eq!(
            parse(&["--cwd", "/work/example"]),
            Err("unexpected argument '/work/example'".to_owned())
        );
        assert_eq!(
            asked(&["--time", "relative"]).time,
            Some(app::TimeStyle::Relative)
        );
        assert_eq!(asked(&["--time=clock"]).time, Some(app::TimeStyle::Clock));
        assert!(parse(&["--time", "yesterday"])
            .unwrap_err()
            .contains("yesterday"));
        assert!(parse(&["--time"]).is_err());

        for (length, seconds) in [
            ("90s", 90),
            ("30m", 1800),
            ("2h", 7200),
            ("2d", 172_800),
            ("45", 45),
            ("0", 0),
        ] {
            assert_eq!(
                asked(&["--ago", length]).ago,
                Some(Duration::from_secs(seconds)),
                "{length}"
            );
        }
        for wrong in ["soon", "1w", "h", "-5m", "1.5h", ""] {
            assert!(parse(&["--ago", wrong]).is_err(), "{wrong}");
        }
        assert!(parse(&["--ago=99999999999999999999d"]).is_err());
    }

    #[test]
    fn a_digit_shows_the_page_the_configuration_puts_there() {
        let mut app = App::new(Config {
            pages: vec![Page::Usage, Page::Sessions],
            start: Page::Usage,
            ..app::tests::fixture_config()
        });
        assert_eq!(app.page, Page::Usage);
        press(&mut app, "2");
        assert_eq!(app.page, Page::Sessions);
        // Past the pages that are shown, nothing.
        press(&mut app, "3");
        assert_eq!(app.page, Page::Sessions);
        // Enter on a session narrows the other lists, but the Tool calls
        // page is not shown: it stays.
        press(&mut app, "\n");
        assert!(app.scope.is_some());
        assert_eq!(app.page, Page::Sessions);
        press(&mut app, "1");
        assert_eq!(app.page, Page::Usage);
    }

    fn press(app: &mut App, keys: &str) {
        let mut after_g = false;
        for c in keys.chars() {
            let (code, modifiers) = match c {
                '\t' => (KeyCode::Tab, KeyModifiers::NONE),
                '\n' => (KeyCode::Enter, KeyModifiers::NONE),
                '\x1b' => (KeyCode::Esc, KeyModifiers::NONE),
                // Ctrl+d and Ctrl+u, written as the control characters.
                '\x04' => (KeyCode::Char('d'), KeyModifiers::CONTROL),
                '\x15' => (KeyCode::Char('u'), KeyModifiers::CONTROL),
                c => (KeyCode::Char(c), KeyModifiers::NONE),
            };
            let handled = handle_key(app, KeyEvent::new(code, modifiers), after_g);
            after_g = handled == Handled::PendingG;
        }
    }

    #[test]
    fn keys_follow_vigs_conventions() {
        let mut app = app::tests::fixture_app();
        assert_eq!((app.page, app.in_detail), (Page::Sessions, false));

        // Moving.
        press(&mut app, "j");
        assert_eq!(app.selected, 1);
        press(&mut app, "G");
        assert_eq!(app.selected, 2);
        press(&mut app, "g");
        assert_eq!(app.selected, 2, "one g waits for the second");
        press(&mut app, "gg");
        assert_eq!(app.selected, 0);
        press(&mut app, "gjg");
        assert_eq!(app.selected, 1, "a g followed by another key is dropped");

        // The detail pane and back.
        press(&mut app, "i");
        assert!(app.in_detail);
        press(&mut app, "h");
        assert!(!app.in_detail);
        press(&mut app, "\t");
        assert!(app.in_detail);
        press(&mut app, "\x1b");
        assert!(!app.in_detail);

        // Enter on a session: the other pages are narrowed to it.
        press(&mut app, "\n");
        assert_eq!(app.page, Page::Tools);
        assert_eq!(app.scope.as_deref(), Some(app::tests::WITH_SUBAGENTS));
        // Enter on any other list is the detail pane.
        press(&mut app, "\n");
        assert!(app.in_detail);
        press(&mut app, "\x1b4");
        assert_eq!((app.page, app.in_detail), (Page::Subagents, false));
        assert!(app.scope.is_some(), "the narrowing holds across pages");
        // Esc on a list clears it.
        press(&mut app, "\x1b");
        assert_eq!(app.scope, None);

        // Search: typed keys are text until Enter.
        press(&mut app, "2/sq1\n");
        assert_eq!(app.search.as_ref().unwrap().query, "sq1");
        assert_eq!(
            app.page,
            Page::Sessions,
            "the 1 was typed, not a page switch"
        );
        press(&mut app, "\x1b");
        assert_eq!(app.search, None);
        press(&mut app, "/abc\x1b");
        assert_eq!((app.typing.clone(), app.search.clone()), (None, None));

        // Sort and filter belong to the Sessions page.
        press(&mut app, "s");
        assert_eq!(app.sort, app::Sort::Cost);
        press(&mut app, "f");
        assert_eq!(app.filter, app::Filter::Live);
        press(&mut app, "ff3sf");
        assert_eq!(
            (app.page, app.sort, app.filter),
            (Page::Tools, app::Sort::Cost, app::Filter::All)
        );
        press(&mut app, "5j\ti");
        assert_eq!((app.page, app.in_detail), (Page::Usage, false));

        // The Trace page: `l` and `h` are the pane on the right and back,
        // `t` the list by session, Enter a fold.
        app.by_session = false;
        press(&mut app, "1l");
        assert!(app.in_call() && app.from_list);
        press(&mut app, "h");
        assert!(!app.in_detail);
        press(&mut app, "t");
        assert!(app.by_session);
        // The cursor stayed on its interaction: up to its session.
        press(&mut app, "k\n");
        assert_eq!(app.closed_sessions.len(), 1);
        press(&mut app, "\nt");
        assert!(app.closed_sessions.is_empty() && !app.by_session);
        press(&mut app, "j\njl");
        assert!(app.in_call() && !app.from_list);
        // Out of the pane, up to the response that made the call, out of
        // the tree.
        press(&mut app, "hhh");
        assert!(!app.in_detail);

        // `T` switches how times are shown.
        press(&mut app, "T");
        assert_eq!(app.time, app::TimeStyle::Relative);
        press(&mut app, "T");
        assert_eq!(app.time, app::TimeStyle::Clock);

        // `o` reads the whole output of the call shown, and drops it again.
        press(&mut app, "3o");
        assert!(app.whole.is_some());
        press(&mut app, "o");
        assert!(app.whole.is_none());

        // Help.
        press(&mut app, "1?");
        assert!(app.help);
        press(&mut app, "q");
        assert!(!app.help, "any key closes the help, and is not acted on");

        let quit = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            false,
        );
        assert_eq!(quit, Handled::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.typing = Some(String::new());
        assert_eq!(handle_key(&mut app, ctrl_c, false), Handled::Quit);
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

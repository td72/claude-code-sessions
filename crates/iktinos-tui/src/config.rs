//! The configuration file: `~/.config/iktinos/config.toml`, or the one
//! `--config` names.
//!
//! Three sources decide how the viewer behaves, in this order: the
//! command-line arguments, the file, the defaults ([`Config::new`]). The
//! file is only ever read. A key it does not know, a value of the wrong
//! type, a length of time or a name that cannot be read: each stops the
//! viewer at startup with the line it is on, since a setting that is
//! silently ignored is one nobody notices was misspelt.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::Deserialize;
use toml::Spanned;

use crate::app::{parse_duration, Config, Filter, Page, Sort, TimeStyle};

/// What the command line asked for. `None` and `false` leave the choice to
/// the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// `--config`: the file to read instead of the default one.
    pub config: Option<PathBuf>,
    pub projects_dir: Option<PathBuf>,
    pub socket_dir: Option<PathBuf>,
    pub no_live: bool,
    pub time: Option<TimeStyle>,
    pub ago: Option<Duration>,
    pub cwd: bool,
}

/// What the viewer cannot decide from its arguments and its file.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// For a leading `~` in a path.
    pub home: Option<PathBuf>,
    /// For `--cwd`.
    pub current_dir: Option<PathBuf>,
    /// Where Claude Code keeps its transcripts, unless told otherwise.
    pub default_projects_dir: Option<PathBuf>,
}

/// The file as it is written. Everything may be left out.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    projects_dir: Option<PathBuf>,
    cwd: Option<bool>,
    refresh: Option<Spanned<Length>>,
    #[serde(default)]
    pages: Pages,
    #[serde(default)]
    time: Time,
    #[serde(default)]
    live: Live,
    #[serde(default)]
    sessions: Sessions,
    #[serde(default)]
    trace: Trace,
    #[serde(default)]
    tools: Tools,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pages {
    order: Option<Spanned<Vec<PageName>>>,
    start: Option<Spanned<PageName>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Time {
    style: Option<StyleName>,
    ago: Option<Length>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Live {
    enabled: Option<bool>,
    socket_dir: Option<PathBuf>,
    quiet_after: Option<Length>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sessions {
    sort: Option<SortName>,
    filter: Option<FilterName>,
    active_within: Option<Length>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    by_session: Option<bool>,
    interactions: Option<usize>,
    slow: Option<Spanned<[Length; 2]>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tools {
    recent: Option<usize>,
    output_chars: Option<usize>,
}

/// A length of time, written as `--ago` takes it: `90s`, `30m`, `1h`, `2d`.
#[derive(Debug, Clone, Copy)]
struct Length(Duration);

impl<'de> Deserialize<'de> for Length {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = Length;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a length of time such as \"90s\", \"30m\" or \"1h\"")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Length, E> {
                parse_duration(text)
                    .map(Length)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PageName {
    Trace,
    Sessions,
    Tools,
    Subagents,
    Usage,
}

impl PageName {
    fn page(self) -> Page {
        match self {
            Self::Trace => Page::Trace,
            Self::Sessions => Page::Sessions,
            Self::Tools => Page::Tools,
            Self::Subagents => Page::Subagents,
            Self::Usage => Page::Usage,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StyleName {
    Clock,
    Relative,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SortName {
    Updated,
    Cost,
    Tokens,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FilterName {
    All,
    Live,
    Open,
}

/// Where the file is unless `--config` names one:
/// `$XDG_CONFIG_HOME/iktinos/config.toml`, else
/// `~/.config/iktinos/config.toml`. `None` without either directory.
pub fn default_path(xdg_config_home: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    let dir = xdg_config_home
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".config")))?;
    Some(dir.join("iktinos").join("config.toml"))
}

/// Reads the file at `path`. One that is not there is an error only if it
/// was asked for by name (`named`); otherwise every setting is left to its
/// default.
pub fn load(path: &Path, named: bool) -> Result<File, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !named => Ok(File::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Reads the text of a file, and checks what the types alone do not.
pub fn parse(text: &str) -> Result<File, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string().trim_end().to_owned())?;
    // Where in the text something that was read is.
    let line = |span: std::ops::Range<usize>| {
        let at = span.start.min(text.len());
        text[..at].matches('\n').count() + 1
    };

    if let Some(order) = &file.pages.order {
        let at = line(order.span());
        let pages = order.get_ref();
        if pages.is_empty() {
            return Err(format!("line {at}: pages.order lists no page"));
        }
        let mut seen = HashSet::new();
        if let Some(twice) = pages.iter().find(|page| !seen.insert(**page)) {
            return Err(format!(
                "line {at}: pages.order lists {} twice",
                twice.page().name()
            ));
        }
    }
    if let Some(start) = &file.pages.start {
        let shown = match &file.pages.order {
            Some(order) => order.get_ref().contains(start.get_ref()),
            None => true,
        };
        if !shown {
            return Err(format!(
                "line {}: pages.start is {}, which pages.order does not list",
                line(start.span()),
                start.get_ref().page().name()
            ));
        }
    }
    if let Some(refresh) = &file.refresh {
        if refresh.get_ref().0.is_zero() {
            return Err(format!(
                "line {}: refresh must be longer than nothing",
                line(refresh.span())
            ));
        }
    }
    if let Some(slow) = &file.trace.slow {
        let [first, second] = slow.get_ref();
        if first.0 > second.0 {
            return Err(format!(
                "line {}: trace.slow is two lengths, the shorter first",
                line(slow.span())
            ));
        }
    }
    Ok(file)
}

/// The settings the viewer runs with: an argument where one was given,
/// else the file's, else the default.
pub fn resolve(args: Args, file: File, env: &Env) -> Result<Config, String> {
    let home = |path: PathBuf| match (path.strip_prefix("~"), &env.home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    };
    let projects_dir = args
        .projects_dir
        .or(file.projects_dir.map(home))
        .or_else(|| env.default_projects_dir.clone())
        .ok_or("no home directory; pass --projects-dir")?;
    let mut config = Config::new(projects_dir);

    if args.cwd || file.cwd == Some(true) {
        let dir = env
            .current_dir
            .clone()
            .ok_or("--cwd: cannot tell the current directory")?;
        config.within = Some(dir);
    }
    if let Some(refresh) = file.refresh {
        config.refresh = refresh.into_inner().0;
    }

    if let Some(order) = file.pages.order {
        config.pages = order.into_inner().into_iter().map(PageName::page).collect();
    }
    // The first page, unless another is named.
    config.start = match file.pages.start {
        Some(start) => start.into_inner().page(),
        None => config.pages[0],
    };

    if let Some(style) = file.time.style {
        config.time = match style {
            StyleName::Clock => TimeStyle::Clock,
            StyleName::Relative => TimeStyle::Relative,
        };
    }
    if let Some(time) = args.time {
        config.time = time;
    }
    if let Some(ago) = args.ago.or(file.time.ago.map(|ago| ago.0)) {
        config.ago_within = ago;
    }

    config.live = !args.no_live && file.live.enabled.unwrap_or(true);
    config.socket_dir = args.socket_dir.or(file.live.socket_dir.map(home));
    if let Some(quiet) = file.live.quiet_after {
        config.quiet_after = quiet.0;
    }

    if let Some(sort) = file.sessions.sort {
        config.sort = match sort {
            SortName::Updated => Sort::Updated,
            SortName::Cost => Sort::Cost,
            SortName::Tokens => Sort::Tokens,
        };
    }
    if let Some(filter) = file.sessions.filter {
        config.filter = match filter {
            FilterName::All => Filter::All,
            FilterName::Live => Filter::Live,
            FilterName::Open => Filter::Open,
        };
    }
    if let Some(active) = file.sessions.active_within {
        config.active_within = active.0;
    }

    if let Some(by_session) = file.trace.by_session {
        config.by_session = by_session;
    }
    if let Some(interactions) = file.trace.interactions {
        config.interactions = interactions;
    }
    if let Some(slow) = file.trace.slow {
        let [first, second] = slow.into_inner();
        config.slow = [first.0, second.0];
    }

    if let Some(recent) = file.tools.recent {
        config.recent_tools = recent;
    }
    if let Some(chars) = file.tools.output_chars {
        config.output_chars = chars;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DEFAULT_AGO_WITHIN;

    fn env() -> Env {
        Env {
            home: Some("/home/synthetic".into()),
            current_dir: Some("/work/example".into()),
            default_projects_dir: Some("/home/synthetic/.claude/projects".into()),
        }
    }

    fn config(text: &str, args: Args) -> Config {
        resolve(args, parse(text).unwrap(), &env()).unwrap()
    }

    /// Every key, each with a value that is not its default.
    const FULL: &str = r#"
projects_dir = "~/elsewhere/projects"
cwd = true
refresh = "5s"

[pages]
order = ["sessions", "trace", "usage"]
start = "trace"

[time]
style = "relative"
ago = "30m"

[live]
enabled = false
socket_dir = "~/sockets"
quiet_after = "2m"

[sessions]
sort = "cost"
filter = "open"
active_within = "90s"

[trace]
by_session = false
interactions = 5
slow = ["3s", "20s"]

[tools]
recent = 50
output_chars = 300
"#;

    #[test]
    fn without_a_file_everything_is_its_default() {
        let config = config("", Args::default());
        assert_eq!(
            config,
            Config::new("/home/synthetic/.claude/projects".into())
        );
        assert_eq!(config.pages, Page::ALL);
        assert_eq!(config.start, Page::Trace);
        assert!(config.live && config.by_session);
        assert_eq!(config.within, None);
        assert_eq!(config.refresh, Duration::from_secs(1));
        assert_eq!(config.ago_within, DEFAULT_AGO_WITHIN);
        assert_eq!(
            config.slow,
            [Duration::from_secs(10), Duration::from_secs(60)]
        );
        assert_eq!((config.interactions, config.recent_tools), (20, 200));
        assert_eq!(config.output_chars, 2000);

        // A file that is not there is the same, unless it was named.
        let missing = Path::new("/no-such-root/iktinos/config.toml");
        assert!(load(missing, false).is_ok());
        let named = load(missing, true).unwrap_err();
        assert!(
            named.starts_with("/no-such-root/iktinos/config.toml: "),
            "{named}"
        );
    }

    #[test]
    fn every_key_is_read() {
        let config = config(FULL, Args::default());
        // A leading `~` is the home directory.
        assert_eq!(
            config.projects_dir,
            PathBuf::from("/home/synthetic/elsewhere/projects")
        );
        assert_eq!(config.socket_dir, Some("/home/synthetic/sockets".into()));
        assert_eq!(config.within, Some("/work/example".into()));
        assert_eq!(config.refresh, Duration::from_secs(5));
        assert_eq!(config.pages, [Page::Sessions, Page::Trace, Page::Usage]);
        assert_eq!(config.start, Page::Trace);
        assert_eq!(config.time, TimeStyle::Relative);
        assert_eq!(config.ago_within, Duration::from_secs(1800));
        assert!(!config.live);
        assert_eq!(config.quiet_after, Duration::from_secs(120));
        assert_eq!((config.sort, config.filter), (Sort::Cost, Filter::Open));
        assert_eq!(config.active_within, Duration::from_secs(90));
        assert!(!config.by_session);
        assert_eq!(config.interactions, 5);
        assert_eq!(
            config.slow,
            [Duration::from_secs(3), Duration::from_secs(20)]
        );
        assert_eq!((config.recent_tools, config.output_chars), (50, 300));
    }

    #[test]
    fn an_argument_goes_before_the_file() {
        let args = Args {
            config: None,
            projects_dir: Some("/asked/projects".into()),
            socket_dir: Some("/asked/sockets".into()),
            no_live: true,
            time: Some(TimeStyle::Clock),
            ago: Some(Duration::from_secs(7)),
            cwd: false,
        };
        let config = config(FULL, args);
        assert_eq!(config.projects_dir, PathBuf::from("/asked/projects"));
        assert_eq!(config.socket_dir, Some("/asked/sockets".into()));
        assert_eq!(config.time, TimeStyle::Clock);
        assert_eq!(config.ago_within, Duration::from_secs(7));
        assert!(!config.live);
        // What no argument speaks of stays the file's.
        assert_eq!(config.within, Some("/work/example".into()));
        assert_eq!(config.sort, Sort::Cost);

        // `--no-live` turns off what the file turns on; `--cwd` turns on
        // what the file leaves off.
        let args = Args {
            no_live: true,
            cwd: true,
            ..Args::default()
        };
        let config = self::config("[live]\nenabled = true\n", args);
        assert!(!config.live);
        assert_eq!(config.within, Some("/work/example".into()));
    }

    #[test]
    fn the_first_page_listed_is_where_the_viewer_starts() {
        let config = config("[pages]\norder = [\"usage\", \"tools\"]\n", Args::default());
        assert_eq!(config.pages, [Page::Usage, Page::Tools]);
        assert_eq!(config.start, Page::Usage);
        // Another start, with every page.
        let config = self::config("[pages]\nstart = \"sessions\"\n", Args::default());
        assert_eq!(config.pages, Page::ALL);
        assert_eq!(config.start, Page::Sessions);
    }

    #[test]
    fn what_cannot_be_read_is_an_error_that_names_its_line() {
        let cases = [
            // An unknown key, at the top and in a table.
            ("refresh = \"1s\"\nrefesh = \"2s\"\n", "line 2", "refesh"),
            (
                "[time]\nstyle = \"clock\"\nagoo = \"1h\"\n",
                "line 3",
                "agoo",
            ),
            ("[colours]\nwarn = \"red\"\n", "line 1", "colours"),
            // A value of the wrong type.
            ("cwd = \"yes\"\n", "line 1", "boolean"),
            ("[tools]\nrecent = \"many\"\n", "line 2", "many"),
            ("[tools]\nrecent = -1\n", "line 2", "-1"),
            // A length of time and a name that cannot be read.
            ("\n\nrefresh = \"soon\"\n", "line 3", "soon"),
            ("[time]\nstyle = \"sundial\"\n", "line 2", "sundial"),
            ("[sessions]\nsort = \"size\"\n", "line 2", "size"),
            ("[sessions]\nfilter = \"mine\"\n", "line 2", "mine"),
            ("[pages]\norder = [\"trace\", \"logs\"]\n", "line 2", "logs"),
            // The pages: none, one twice, a start that is not shown.
            ("[pages]\norder = []\n", "line 2", "lists no page"),
            (
                "[pages]\norder = [\"trace\", \"usage\", \"trace\"]\n",
                "line 2",
                "lists trace twice",
            ),
            (
                "[pages]\norder = [\"trace\"]\nstart = \"usage\"\n",
                "line 3",
                "does not list",
            ),
            // Values that make no sense.
            ("refresh = \"0\"\n", "line 1", "longer than nothing"),
            (
                "[trace]\nslow = [\"1m\", \"10s\"]\n",
                "line 2",
                "the shorter first",
            ),
            ("[trace]\nslow = [\"1m\"]\n", "line 2", "slow"),
            // Not TOML at all.
            ("refresh = \n", "line 1", "refresh"),
        ];
        for (text, line, word) in cases {
            let error = parse(text).unwrap_err();
            assert!(
                error.contains(line) && error.contains(word),
                "{text:?}: {error}"
            );
        }
    }

    #[test]
    fn the_file_is_under_the_configuration_directory() {
        let home = Path::new("/home/synthetic");
        assert_eq!(
            default_path(None, Some(home)),
            Some("/home/synthetic/.config/iktinos/config.toml".into())
        );
        assert_eq!(
            default_path(Some("/xdg".into()), Some(home)),
            Some("/xdg/iktinos/config.toml".into())
        );
        assert_eq!(
            default_path(Some("".into()), Some(home)),
            Some("/home/synthetic/.config/iktinos/config.toml".into())
        );
        assert_eq!(default_path(None, None), None);
    }

    #[test]
    fn without_a_home_the_projects_directory_must_be_named() {
        let nowhere = Env::default();
        let error = resolve(Args::default(), File::default(), &nowhere).unwrap_err();
        assert!(error.contains("--projects-dir"), "{error}");
        let args = Args {
            cwd: true,
            projects_dir: Some("/p".into()),
            ..Args::default()
        };
        let error = resolve(args, File::default(), &nowhere).unwrap_err();
        assert!(error.contains("current directory"), "{error}");
    }
}

//! The colours, by what they are for.
//!
//! The drawing code never names a colour: it asks for a role, and the role's
//! colour comes from here. The defaults are the terminal's own sixteen
//! colours, so they follow the terminal's palette; the configuration file's
//! `[colors]` replaces any of them.
//!
//! The colours of a shell command's syntax are not roles: `highlight` has
//! its own table.

use std::cell::Cell;

use ratatui::style::Color;

/// A colour for each role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// The names of fields, the headings inside a pane, the keys in the
    /// help.
    pub label: Color,
    /// The first line of a detail: what it is about.
    pub headline: Color,
    /// The frame of the pane that has the focus.
    pub focus: Color,
    /// A session's name where it heads its interactions, and the mark of
    /// the session the lists are narrowed to.
    pub session: Color,
    /// Where a session runs.
    pub cwd: Color,
    /// A tool's name.
    pub tool: Color,
    /// A subagent.
    pub agent: Color,
    /// An LLM request.
    pub request: Color,
    /// Finished and well; working; plenty of room.
    pub ok: Color,
    /// Wants attention: running, slow, waiting for approval, nearly full.
    pub warn: Color,
    /// Failed, very slow, full, cannot be read.
    pub error: Color,
    /// A session that waits for its user.
    pub waiting: Color,
    /// What matters least: captions, what is absent or unknown.
    pub dim: Color,
    /// A row a search matches, and the search being typed.
    pub search: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            label: Color::Cyan,
            headline: Color::Yellow,
            focus: Color::Cyan,
            session: Color::Cyan,
            cwd: Color::Yellow,
            tool: Color::Cyan,
            agent: Color::Magenta,
            request: Color::LightBlue,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
            waiting: Color::Cyan,
            dim: Color::DarkGray,
            search: Color::Yellow,
        }
    }
}

thread_local! {
    /// The theme of the screen being drawn. Drawing happens on one thread,
    /// and every draw sets it first.
    static CURRENT: Cell<Option<Theme>> = const { Cell::new(None) };
}

/// The theme to draw with.
pub fn current() -> Theme {
    CURRENT.get().unwrap_or_default()
}

/// Sets the theme for what is drawn next on this thread.
pub fn set(theme: Theme) {
    CURRENT.set(Some(theme));
}

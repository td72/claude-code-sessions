//! Colours for a shell command: syntect's Bash grammar, with its scopes
//! mapped to the terminal's own colours rather than a theme's RGB, so the
//! result reads on any background.

use std::cell::RefCell;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Scope prefixes and their styles. For a character, the innermost scope
/// that has one of these prefixes decides; the first prefix listed wins.
const STYLES: &[(&str, Color, bool)] = &[
    ("comment", Color::DarkGray, false),
    ("constant.character.escape", Color::Cyan, false),
    ("constant", Color::Magenta, false),
    ("string", Color::Green, false),
    ("variable.function", Color::Blue, true),
    ("support.function", Color::Blue, true),
    ("variable.parameter", Color::Yellow, false),
    ("variable", Color::Cyan, false),
    ("punctuation.definition.variable", Color::Cyan, false),
    ("keyword.operator", Color::Red, false),
    ("keyword", Color::Magenta, false),
    ("storage", Color::Magenta, false),
];

fn style_of(stack: &ScopeStack) -> Style {
    for scope in stack.as_slice().iter().rev() {
        let name = scope.build_string();
        let found = STYLES.iter().find(|(prefix, _, _)| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        });
        if let Some((_, color, bold)) = found {
            let style = Style::new().fg(*color);
            return if *bold {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            };
        }
    }
    Style::new()
}

/// The style of every character of `command`, line by line (as
/// `str::lines` splits it). Plain where the grammar says nothing, and all of
/// it plain if the grammar is missing or gives up.
pub fn bash(command: &str) -> Vec<Vec<Style>> {
    thread_local! {
        /// The last command coloured: the same one is drawn again and again.
        static LAST: RefCell<Option<(String, Vec<Vec<Style>>)>> = const { RefCell::new(None) };
    }
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        match &*last {
            Some((text, styles)) if text == command => styles.clone(),
            _ => {
                let styles = colour(command);
                *last = Some((command.to_owned(), styles.clone()));
                styles
            }
        }
    })
}

fn colour(command: &str) -> Vec<Vec<Style>> {
    let plain = |line: &str| vec![Style::new(); line.chars().count()];
    let syntaxes = syntaxes();
    let Some(syntax) = syntaxes.find_syntax_by_extension("sh") else {
        return command.lines().map(plain).collect();
    };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut failed = false;
    command
        .lines()
        .map(|line| {
            if failed {
                return plain(line);
            }
            let Ok(ops) = state.parse_line(&format!("{line}\n"), syntaxes) else {
                failed = true;
                return plain(line);
            };
            let mut styles = Vec::with_capacity(line.len());
            let mut at = 0;
            let mut paint = |to: usize, stack: &ScopeStack| {
                let to = to.min(line.len());
                if let Some(text) = line.get(at..to) {
                    styles.extend(std::iter::repeat_n(style_of(stack), text.chars().count()));
                    at = to;
                }
            };
            for (offset, op) in &ops {
                paint(*offset, &stack);
                if stack.apply(op).is_err() {
                    failed = true;
                    break;
                }
            }
            paint(line.len(), &stack);
            // Whatever happened, one style per character.
            styles.resize(line.chars().count(), Style::new());
            styles
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(styles: &[Style], line: &str, word: &str) -> Option<Color> {
        let at = line[..line.find(word).unwrap()].chars().count();
        styles[at].fg
    }

    #[test]
    fn a_command_is_coloured_by_what_its_parts_are() {
        let command = "cargo test --workspace \"$HOME/x\" | grep -v 'ok' # done\necho あ $USER";
        let styles = bash(command);
        let lines: Vec<&str> = command.lines().collect();
        assert_eq!(styles.len(), 2);
        for (line, styles) in lines.iter().zip(&styles) {
            assert_eq!(styles.len(), line.chars().count());
        }
        let first = |word| fg(&styles[0], lines[0], word);
        assert_eq!(first("cargo"), Some(Color::Blue));
        assert_eq!(first("grep"), Some(Color::Blue));
        assert_eq!(first("--workspace"), Some(Color::Yellow));
        assert_eq!(first("HOME"), Some(Color::Cyan));
        assert_eq!(first("/x"), Some(Color::Green));
        assert_eq!(first("ok"), Some(Color::Green));
        assert_eq!(first("# done"), Some(Color::DarkGray));
        assert_eq!(first("test"), None);
        // Characters wider than a byte do not shift what follows.
        assert_eq!(fg(&styles[1], lines[1], "USER"), Some(Color::Cyan));

        // The same again comes from the cache, and is the same.
        assert_eq!(bash(command), styles);
        assert_eq!(bash(""), Vec::<Vec<Style>>::new());
    }

    #[test]
    fn a_command_cut_short_still_has_a_style_per_character() {
        let command = "cat <<'EOF'\nsome text\nmore \"unclosed…";
        let styles = bash(command);
        assert_eq!(styles.len(), 3);
        for (line, styles) in command.lines().zip(&styles) {
            assert_eq!(styles.len(), line.chars().count());
        }
    }
}

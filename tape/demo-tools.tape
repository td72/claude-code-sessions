# Recorded in a container (tape/record.sh) from synthetic sessions:
# tape/demo-fixture.py writes four invented sessions into a scratch
# projects directory, keeps one of them working for a minute and posts the
# live events the bridge mod would send. No real Claude Code session exists
# where this runs.
#
# The Tool calls page: the list, a search, a call's whole output.

Output assets/demo-tools.gif

Require iktinos
Require python3

Set Shell "bash"
Set FontFamily "Hack Nerd Font Mono"
Set FontSize 15
Set Width 1500
Set Height 820
Set Padding 24
Set Theme "Catppuccin Frappe"
Set WindowBar Colorful
Set CursorBlink false
Set TypingSpeed 50ms

# --- Hidden setup: a scratch directory and the synthetic sessions ---
Hide

Type@1ms "cd $(mktemp -d) && export HOME=$PWD"
Enter
Sleep 300ms
# Written now, then appended to while iktinos is watching.
Type@1ms "(python3 /demo/tape/demo-fixture.py projects sockets live &) 2>/dev/null"
Enter
Sleep 1s
Type@1ms "clear"
Enter
Sleep 300ms

Show

# --- Demo ---

Type "iktinos --projects-dir projects --socket-dir sockets"
Enter

Sleep 1500ms

# Tool calls of every session, the latest first, as they happen
Type "3"
Sleep 5s

# Search
Type "/clippy"
Sleep 500ms
Enter
Sleep 2s
Escape
Sleep 1s

# One session's calls: Enter on it in the Sessions page
Type "2"
Sleep 1500ms
Type "j"
Sleep 600ms
Type "j"
Sleep 1s
Enter
Sleep 2s

# The oldest: a test run that failed, and what it printed
Type "G"
Sleep 2500ms
Type "o"
Sleep 3s

Type "q"
Sleep 500ms

# Recorded in a container (tape/record.sh) from synthetic sessions:
# tape/demo-fixture.py writes four invented sessions into a scratch
# projects directory, keeps one of them working for a minute and posts the
# live events the bridge mod would send. No real Claude Code session exists
# where this runs.
#
# The viewer, page by page.

Output assets/demo.gif

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

# Trace: every session with its interactions; one is still running
Sleep 4s

# Sessions: one is working, one waits for a permission
Type "2"
Sleep 3s
Type "j"
Sleep 1200ms
Type "k"
Sleep 1200ms

# Enter: the session's tool calls, as they happen
Enter
Sleep 6s

# Its subagents
Type "4"
Sleep 2500ms

# Its trace: the interaction that is running, as a tree
Type "1"
Sleep 1500ms
Type "j"
Sleep 4s

# Usage, over every session
Type "5"
Sleep 3s

Type "q"
Sleep 500ms

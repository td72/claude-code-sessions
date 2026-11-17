# Recorded in a container (tape/record.sh) from synthetic sessions:
# tape/demo-fixture.py writes four invented sessions into a scratch
# projects directory, keeps one of them working for a minute and posts the
# live events the bridge mod would send. No real Claude Code session exists
# where this runs.
#
# What live events add: the tool that runs, a permission that is waited for.

Output assets/demo-live.gif

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

Sleep 2s

# Sessions: the state column follows the events
Type "2"
Sleep 6s

# This one waits for a permission: only an event can say that
Type "j"
Sleep 6s

# It is allowed, and runs
Sleep 8s
Type "k"
Sleep 4s

# The help ends with what the socket has heard
Type "?"
Sleep 3s
Escape
Sleep 500ms

Type "q"
Sleep 500ms

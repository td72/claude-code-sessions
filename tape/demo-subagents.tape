# Recorded in a container (tape/record.sh) from synthetic sessions:
# tape/demo-fixture.py writes four invented sessions into a scratch
# projects directory, keeps one of them working for a minute and posts the
# live events the bridge mod would send. No real Claude Code session exists
# where this runs.
#
# The Subagents page: a subagent, its calls, narrowing to it.

Output assets/demo-subagents.gif

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

Type "4"
Sleep 3s

# Into the detail: choose one of its calls
Type "i"
Sleep 1s
Type "j"
Sleep 700ms
Type "j"
Sleep 1500ms
# The call, in its own pane
Enter
Sleep 2500ms
Escape
Sleep 500ms
Escape
Sleep 1s

# Another subagent; Enter narrows the Tool calls page to it
Type "j"
Sleep 2s
Enter
Sleep 3s

Type "q"
Sleep 500ms

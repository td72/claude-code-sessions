# Recorded in a container (tape/record.sh) from synthetic sessions:
# tape/demo-fixture.py writes four invented sessions into a scratch
# projects directory, keeps one of them working for a minute and posts the
# live events the bridge mod would send. No real Claude Code session exists
# where this runs.
#
# The Trace page: sessions, interactions, an interaction as a tree.

Output assets/demo-trace.gif

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

# On a session's row: the session beside the list
Sleep 3s

# The interaction that is running: its tree grows
Type "j"
Sleep 4s

# An earlier one, with a subagent
Type "j"
Sleep 800ms
Type "j"
Sleep 2s

# Into its tree: requests, the calls under them, and the subagent
Enter
Sleep 1s
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Type "j"
Sleep 1500ms
# Unfold the subagent
Enter
Sleep 2500ms
Type "j"
Sleep 600ms
Type "j"
Sleep 1500ms
# The pane on the right, and back
Type "l"
Sleep 2s
Type "h"
Sleep 1s

# Back to the list; `t` lists the interactions all together
Escape
Sleep 500ms
Escape
Sleep 1s
Type "t"
Sleep 3s
Type "t"
Sleep 1500ms

Type "q"
Sleep 500ms

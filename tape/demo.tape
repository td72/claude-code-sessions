# Recorded in a container (tape/record.sh) from synthetic sessions:
# tape/demo-fixture.py writes four invented sessions into a scratch
# projects directory, keeps one of them working for a minute and posts the
# live events the bridge mod would send. No real Claude Code session exists
# where this runs.

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
Sleep 4s

# Sessions: one is working, one waits for a permission
Type "j"
Sleep 1.5s
Type "j"
Sleep 1.5s
Type "gg"
Sleep 1s

# Enter: its tool calls, as they happen
Enter
Sleep 6s

# Subagents
Type "3"
Sleep 2.5s

# Trace: the interactions of the session, and one as a tree
Type "4"
Sleep 2s
Type "j"
Sleep 800ms
Type "j"
Sleep 800ms
Type "j"
Sleep 2s
Enter
Sleep 1s
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Type "j"
Sleep 1s
# Unfold the subagent
Enter
Sleep 2.5s
Type "j"
Sleep 600ms
Type "j"
Sleep 2s

# Back out, to every session again
Escape
Sleep 500ms
Escape
Sleep 500ms
Escape
Sleep 1s

# Usage
Type "5"
Sleep 3s

# Help
Type "?"
Sleep 3s
Escape
Sleep 500ms

Type "q"
Sleep 500ms

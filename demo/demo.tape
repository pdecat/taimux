# The README's demo recording, driven by vhs (https://github.com/charmbracelet/vhs).
#
#   mise x vhs -- vhs demo/demo.tape
#
# It records `demo/demo.sh`, which builds an ISOLATED tmux server (-L
# taimux-demo) full of synthetic agent sessions. Nothing here touches, reads or
# can record a real session: the projects, the task summaries and the agent
# binaries are all fabricated by that script. That property is the whole reason
# the recording goes through the demo rather than through a real picker, and it
# is worth keeping if this tape is ever edited.
#
# Timings are generous on purpose. The picker refreshes on a timer and reads
# pane screens on each pass, so a Sleep that is too short records a half-drawn
# list, which looks like a bug rather than like a demo.

Output demo/demo.gif

Set Shell bash
Set FontSize 13
Set Width 1400
Set Height 620
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set PlaybackSpeed 1.0

# Everything up to the picker opening is hidden. The demo script builds seven
# synthetic panes across two sessions and a fake remote host, which takes a few
# seconds of blank terminal, and a quarter of the recording spent watching a
# tmux server start is a quarter nobody watches.
Hide
Type "cd $(git rev-parse --show-toplevel) && clear"  Enter
Sleep 1s
Type "demo/demo.sh"  Enter
Sleep 8s

# prefix + a, the binding taimux installs.
Ctrl+a
Type "a"
Sleep 3s
Show

# The list as it opens: every pane running an agent, what it is doing, where,
# and which version that pane is actually running. Two rows come from another
# host, over ssh, discovered from a pane that was already connected to it.
Sleep 2500ms

# Typing filters. "auth" matches two projects in different sessions.
Type "auth"
Sleep 3s
Ctrl+u
Sleep 1500ms

# Tab cycles the list through the states: waiting, working, idle.
Tab
Sleep 2500ms
Tab
Sleep 2500ms
Tab
Sleep 2500ms
Tab
Sleep 2s

# Back to the whole list, move around, and jump.
Tab
Sleep 1500ms
Down
Sleep 700ms
Down
Sleep 700ms
Down
Sleep 1s
Enter
Sleep 3s

# Tear the demo server down.
Hide
Type "exit"  Enter
Sleep 2s

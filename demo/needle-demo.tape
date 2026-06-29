# VHS tape to record the needle-vs-traversal demo GIF.
# Requires: https://github.com/charmbracelet/vhs  (winget install charmbracelet.vhs)
# and the needle daemon already running (./start-daemon.ps1).
#
# Record with:   vhs demo/needle-demo.tape
# Produces:      demo/needle-demo.gif

Output demo/needle-demo.gif

Set Shell "pwsh"
Set FontSize 18
Set Width 1280
Set Height 760
Set Theme "Catppuccin Mocha"
Set Padding 24
Set TypingSpeed 60ms

# Settle.
Sleep 1s

# The pitch, typed live.
Type "# needle: find ANY file on the whole machine, in <1ms" Sleep 700ms Enter
Sleep 1s

# Single whole-drive query against the warm MFT index.
Type "needle find '**/*.rs' --root 'D:\' --max-results 5" Sleep 500ms Enter
Sleep 3s

# Head-to-head against traversal tools on the same whole-drive search.
Type "./demo/compare.ps1 -Ext rs -Drive D" Sleep 500ms Enter
# The first run warms the index, then times every tool. Give it room.
Sleep 12s

# Let the final speedup table breathe.
Sleep 3s

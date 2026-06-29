# VHS tape to record the needle-vs-traversal demo GIF.
# Requires: https://github.com/charmbracelet/vhs  (winget install charmbracelet.vhs)
# and the needle daemon already running (needle service install, or needle serve).
#
# Record with:   vhs demo/needle-demo.tape
# Produces:      demo/needle-demo.gif
#
# Note: queries below target system locations (C:\, *.dll, cmd.exe) on purpose,
# so the recording never exposes personal files or folder names.

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
Type "# needle: find any file across a 2M-file drive, in milliseconds" Sleep 700ms Enter
Sleep 1s

# Whole-drive query against the warm MFT index. cmd.exe lives in system dirs,
# so the results are neutral C:\Windows paths -- no personal files on screen.
Type "needle find '**/cmd.exe' --root 'C:\' --max-results 5" Sleep 500ms Enter
Sleep 3s

# Head-to-head against traversal tools on the same whole-drive search.
# compare.ps1 prints only match counts and timings -- never file paths -- so the
# drive choice here is purely about a fast, dramatic benchmark, not privacy.
Type "./demo/compare.ps1 -Ext rs -Drive D" Sleep 500ms Enter
# The run warms the index then times every tool; ~13s on a 2M-file drive.
Sleep 14s

# Hold on the final speedup table so it's readable before the GIF loops.
Sleep 7s

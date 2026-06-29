# VHS tape: records a STAGED mock of an agent (Claude Code) using needle's
# fast_glob tool to find every mp3 on the machine and organize them.
#
# It runs demo/mock-agent.ps1, which only PRINTS a fabricated transcript -- it
# does not search, move, or delete any real files. Safe to record anytime.
#
# Requires: vhs (winget install charmbracelet.vhs), plus ttyd + ffmpeg on PATH.
# Record with:   vhs demo/needle-agent-demo.tape
# Produces:      demo/needle-agent-demo.gif

Output demo/needle-agent-demo.gif

Set Shell "pwsh"
Set FontSize 18
Set Width 1280
Set Height 760
Set Theme "Catppuccin Mocha"
Set Padding 24
Set TypingSpeed 55ms

# Settle.
Sleep 1s

# Kick off the staged session. The script paces itself with internal pauses,
# so the recording reveals the transcript step by step.
Type "./demo/mock-agent.ps1" Sleep 400ms Enter

# mock-agent.ps1 self-paces (~10s of internal Start-Sleep). Give it room to
# finish printing, then hold on the final "Done" so it's readable on loop.
Sleep 12s
Sleep 5s

# Staged mock of a Claude Code session that uses needle's `fast_glob` tool to
# find every mp3 on the machine and organize them into one folder.
#
# IMPORTANT: this is a DEMO SCRIPT. It does NOT search the filesystem and does
# NOT move, create, or delete anything. Every path and number below is fabricated
# for the recording. It only prints text (with pauses) so `vhs` can capture a
# realistic-looking agent transcript without touching real files.

$ErrorActionPreference = "Stop"

function Pause($ms) { Start-Sleep -Milliseconds $ms }
function User($t)   { Write-Host "› $t" -ForegroundColor White }
function Claude($t) { Write-Host "● $t" -ForegroundColor Magenta }
function Tool($t)   { Write-Host "  ⎿ $t" -ForegroundColor Cyan }
function Dim($t)    { Write-Host "     $t" -ForegroundColor DarkGray }
function Ok($t)     { Write-Host "● $t" -ForegroundColor Green }

Pause 600
User  "Find every mp3 on my machine and move them into D:\Music\All"
Pause 1200

Claude "I'll locate them with the fast_glob MCP tool (needle), then move each file."
Pause 900

# --- the tool call: fast_glob over the whole machine ---
Write-Host "● fast_glob" -ForegroundColor Magenta -NoNewline
Write-Host '(pattern: "**/*.mp3", root: "")' -ForegroundColor DarkGray
Pause 700
Tool "1,284 files across C:, D:  ·  4.2 ms"
Dim  "D:\CloudMusic\YOASOBI - Romance.mp3"
Dim  "D:\CloudMusic\RADWIMPS - 前前前世 (movie ver.).mp3"
Dim  "D:\CloudMusic\Nirvana - Smells Like Teen Spirit.mp3"
Dim  "D:\CloudMusic\手嶌葵 - さよならの夏～コクリコ坂から～.mp3"
Dim  "... 1,280 more"
Pause 1400

Claude "Found 1,284 files. Creating D:\Music\All and moving them there:"
Pause 800
Write-Host "  New-Item -ItemType Directory D:\Music\All" -ForegroundColor Yellow
Pause 400
Write-Host "  Move-Item 'D:\CloudMusic\YOASOBI - Romance.mp3'   D:\Music\All\" -ForegroundColor Yellow
Write-Host "  Move-Item 'D:\CloudMusic\RADWIMPS - 前前前世 (movie ver.).mp3'   D:\Music\All\" -ForegroundColor Yellow
Write-Host "  ... 1,282 more" -ForegroundColor Yellow
Pause 1600

Ok "Done — 1,284 mp3s organized into D:\Music\All."
Pause 400
Dim "Whole-machine scan + plan in under a second, no directory walk."
Pause 800

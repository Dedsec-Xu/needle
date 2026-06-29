# Head-to-head: needle (NTFS MFT index) vs traversal-based search tools, doing
# the SAME whole-drive lookup. Prints a timing table with speedup factors.
#
# Prereq: the needle daemon must be running (start-daemon.ps1). The daemon reads
# the MFT once; `needle find` then answers from the warm in-memory index.
#
# Usage:
#   ./demo/compare.ps1                 # find *.rs on D:
#   ./demo/compare.ps1 -Ext dll -Drive C
#
# It auto-detects competitors among: fd, fff, ripgrep, plus a PowerShell-native
# recursive walk (always available) as the traversal baseline.

param(
    [string]$Ext   = "rs",
    [string]$Drive = "D",
    [int]$Max      = 200,
    [string]$Addr  = "127.0.0.1:48923"
)

$ErrorActionPreference = "SilentlyContinue"
$needle = Join-Path $PSScriptRoot "..\target\release\needle.exe"
$root   = "${Drive}:\"
$glob   = "**/*.$Ext"

function Write-Head($t) { Write-Host "`n$t" -ForegroundColor Cyan }

if (-not (Test-Path $needle)) { Write-Error "needle.exe not found. Run: cargo build --release"; exit 1 }

Write-Head "needle vs traversal -- find *.$Ext across all of ${Drive}:\"
Write-Host "(whole-drive search; lower is better)`n" -ForegroundColor DarkGray

# --- Make sure the daemon is up and warm (this first query may build the index) ---
Write-Host "Warming needle index (one-time MFT read in the daemon)..." -ForegroundColor DarkGray
$null = & $needle find $glob --root $root --max-results 1 --addr $Addr 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Error "needle daemon not reachable at $Addr. Start it first:  ./start-daemon.ps1"
    exit 1
}

$results = [System.Collections.Generic.List[object]]::new()

function Measure-Tool($name, $detail, [scriptblock]$run) {
    $count = 0
    $t = Measure-Command { $count = (& $run | Measure-Object).Count }
    $ms = [math]::Round($t.TotalMilliseconds, 2)
    $results.Add([pscustomobject]@{ Tool = $name; Detail = $detail; Matches = $count; Ms = $ms })
    Write-Host ("  {0,-22} {1,8} matches   {2,10} ms" -f $name, $count, $ms)
}

# --- needle (warm) ---
Measure-Tool "needle (MFT index)" "needle find" {
    & $needle find $glob --root $root --max-results $Max --addr $Addr 2>$null
}

# --- fd, if installed ---
if (Get-Command fd -ErrorAction SilentlyContinue) {
    Measure-Tool "fd (traversal)" "fd -e $Ext" {
        fd -uu -e $Ext . $root 2>$null
    }
}

# --- fff, if installed (best-effort glob over the drive) ---
if (Get-Command fff -ErrorAction SilentlyContinue) {
    Measure-Tool "fff (traversal)" "fff glob" {
        fff --glob "*.$Ext" $root 2>$null
    }
}

# --- ripgrep --files, if installed ---
if (Get-Command rg -ErrorAction SilentlyContinue) {
    Measure-Tool "ripgrep (--files)" "rg --files | match" {
        rg --files $root 2>$null | Select-String -SimpleMatch ".$Ext"
    }
}

# --- PowerShell-native recursive walk (always available baseline) ---
Measure-Tool "PowerShell walk" "Get-ChildItem -Recurse" {
    Get-ChildItem $root -Recurse -File -Filter "*.$Ext" -ErrorAction SilentlyContinue
}

# --- Summary table with speedup vs needle ---
$needleMs = ($results | Where-Object { $_.Tool -like "needle*" }).Ms
Write-Head "Speedup vs needle"
foreach ($r in $results) {
    $factor = if ($needleMs -gt 0) { [math]::Round($r.Ms / $needleMs, 0) } else { 1 }
    $label  = if ($r.Tool -like "needle*") { "baseline" } else { "${factor}x slower" }
    Write-Host ("  {0,-22} {1,10} ms   {2}" -f $r.Tool, $r.Ms, $label)
}
Write-Host ""

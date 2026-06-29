# Head-to-head: needle (NTFS MFT index) vs other file-search tools doing the
# SAME whole-drive lookup. Prints a timing table with speedup factors.
#
# Prereq: the needle daemon must be running (start-daemon.ps1). The daemon reads
# the MFT once; `needle find` then answers from the warm in-memory index.
#
# Usage:
#   ./demo/compare.ps1                 # find *.rs on D:
#   ./demo/compare.ps1 -Ext dll -Drive C
#
# Competitors are auto-detected; whatever is installed gets measured:
#   * es.exe   - Everything's CLI (also MFT-based; the apples-to-apples peer)
#   * fd       - sharkdp/fd (parallel directory traversal)
#   * rg       - ripgrep --files (traversal)
#   * dir /s   - cmd.exe builtin recursive listing
#   * PowerShell Get-ChildItem -Recurse (always available baseline)
#
# Note: fff is intentionally absent -- it is a library/MCP server, not a CLI,
# so it cannot be invoked from the shell for a fair comparison.

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
function Have($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

if (-not (Test-Path $needle)) { Write-Error "needle.exe not found. Run: cargo build --release"; exit 1 }

Write-Head "needle vs the field -- find *.$Ext across all of ${Drive}:\"
Write-Host "(whole-drive search; lower is better)`n" -ForegroundColor DarkGray

# --- Ensure the daemon is up and warm (first query may build the index) ---
Write-Host "Warming needle index (one-time MFT read in the daemon)..." -ForegroundColor DarkGray
$null = & $needle find $glob --root $root --max-results 1 --addr $Addr 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Error "needle daemon not reachable at $Addr. Start it first:  ./start-daemon.ps1"
    exit 1
}

$results = [System.Collections.Generic.List[object]]::new()

function Measure-Tool($name, [scriptblock]$run) {
    $count = 0
    $t = Measure-Command { $count = (& $run | Measure-Object).Count }
    $ms = [math]::Round($t.TotalMilliseconds, 2)
    $results.Add([pscustomobject]@{ Tool = $name; Matches = $count; Ms = $ms })
    Write-Host ("  {0,-26} {1,8} matches   {2,11} ms" -f $name, $count, $ms)
}

# --- needle (warm MFT index) ---
Measure-Tool "needle (MFT index)" {
    & $needle find $glob --root $root --max-results $Max --addr $Addr 2>$null
}

# --- Everything es.exe (also MFT) ---
if (Have es) {
    Measure-Tool "es.exe (Everything, MFT)" {
        es -path $root "*.$Ext" 2>$null
    }
}

# --- fd (traversal) ---
if (Have fd) {
    Measure-Tool "fd (traversal)" {
        fd -uu -e $Ext . $root 2>$null
    }
}

# --- ripgrep --files (traversal) ---
if (Have rg) {
    Measure-Tool "ripgrep (--files)" {
        rg --files $root 2>$null | Select-String -SimpleMatch ".$Ext"
    }
}

# --- cmd.exe dir /s /b (builtin) ---
Measure-Tool "cmd dir /s /b" {
    cmd /c "dir /s /b `"$root*.$Ext`"" 2>$null
}

# --- PowerShell-native recursive walk (always-available baseline) ---
Measure-Tool "PowerShell walk" {
    Get-ChildItem $root -Recurse -File -Filter "*.$Ext" -ErrorAction SilentlyContinue
}

# --- Summary: speedup vs needle ---
$needleMs = ($results | Where-Object { $_.Tool -like "needle*" }).Ms
Write-Head "Speedup vs needle"
foreach ($r in $results) {
    $factor = if ($needleMs -gt 0) { [math]::Round($r.Ms / $needleMs, 0) } else { 1 }
    $label  = if ($r.Tool -like "needle*") { "baseline" } else { "${factor}x slower" }
    Write-Host ("  {0,-26} {1,11} ms   {2}" -f $r.Tool, $r.Ms, $label)
}
Write-Host ""

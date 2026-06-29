# Runs `needle bench` elevated (required to read the NTFS MFT) and writes the
# Markdown result to bench-result.md for pasting into the README.
#
# Usage:  ./bench.ps1            # benchmark drive C
#         ./bench.ps1 D          # benchmark drive D

param([string]$Drive = "C")

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "target\release\needle.exe"
$out = Join-Path $PSScriptRoot "bench-result.md"

if (-not (Test-Path $exe)) {
    Write-Error "needle.exe not found. Run: cargo build --release"
    exit 1
}

$isAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $isAdmin) {
    Write-Host "Elevating to run the benchmark..."
    # Re-launch this script elevated, passing the drive through.
    Start-Process -FilePath "powershell" -Verb RunAs -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", "`"$PSCommandPath`"", $Drive
    )
    exit 0
}

& $exe bench $Drive | Tee-Object -FilePath $out
Write-Host ""
Write-Host "Saved to $out"

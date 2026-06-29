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
    Write-Host "Elevating to run the benchmark (a UAC prompt will appear)..."
    # Re-launch this script elevated and WAIT for it, so we can show the result.
    $p = Start-Process -FilePath "powershell" -Verb RunAs -Wait -PassThru -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", "`"$PSCommandPath`"", $Drive
    )
    if (Test-Path $out) {
        Write-Host ""
        Write-Host "===== bench-result.md ====="
        Get-Content -Path $out -Encoding utf8 | Write-Host
        Write-Host "==========================="
        Write-Host "Saved to $out"
    } else {
        Write-Warning "No result file produced. The elevated run may have failed."
    }
    exit 0
}

# Elevated branch: run the benchmark, write UTF-8, and keep the result on disk.
$result = & $exe bench $Drive
$result | Out-File -FilePath $out -Encoding utf8
$result | Write-Host

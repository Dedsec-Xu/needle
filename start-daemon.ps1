# Starts the needle daemon elevated (required to read the NTFS MFT).
# Self-elevates via UAC if not already running as administrator.

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "target\release\needle.exe"

if (-not (Test-Path $exe)) {
    Write-Error "needle.exe not found. Run: cargo build --release"
    exit 1
}

$isAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $isAdmin) {
    Write-Host "Elevating to administrator..."
    Start-Process -FilePath $exe -ArgumentList "serve" -Verb RunAs
    Write-Host "Daemon launched in an elevated window."
} else {
    & $exe serve
}

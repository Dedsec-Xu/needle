# Installs needle as a Windows service (LocalSystem, auto-start at boot).
# Self-elevates via UAC; after this you never have to start the daemon manually.
#
# Uninstall with:  ./uninstall-service.ps1

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
    Write-Host "Elevating to install the service..."
    Start-Process -FilePath $exe -ArgumentList "service install" -Verb RunAs -Wait
    Write-Host "Done. The 'needled' service is installed and will auto-start at boot."
} else {
    & $exe service install
}

# Stops and removes the needle Windows service. Self-elevates via UAC.

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "target\release\needle.exe"

if (-not (Test-Path $exe)) {
    Write-Error "needle.exe not found."
    exit 1
}

$isAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $isAdmin) {
    Write-Host "Elevating to remove the service..."
    Start-Process -FilePath $exe -ArgumentList "service uninstall" -Verb RunAs -Wait
    Write-Host "Done. The 'needled' service has been removed."
} else {
    & $exe service uninstall
}

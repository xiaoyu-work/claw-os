# install.ps1 — Helper to import claw-os into WSL2 on Windows.
#
# Usage (run in PowerShell):
#   .\install.ps1                                            # default settings
#   .\install.ps1 -DistroName claw-os-dev                    # custom name
#   .\install.ps1 -InstallPath D:\WSL\claw-os                # custom location
#   .\install.ps1 -Tarball .\claw-os-wsl-amd64.tar.gz        # explicit tarball
#
# Requirements:
#   - Windows 10 21H2+ or Windows 11
#   - WSL2 enabled (`wsl --install` if not yet)

[CmdletBinding()]
param(
    [string]$DistroName  = "claw-os",
    [string]$InstallPath = "$env:LOCALAPPDATA\WSL\claw-os",
    [string]$Tarball     = "$PSScriptRoot\..\..\build\claw-os-wsl-amd64.tar.gz"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Tarball)) {
    Write-Error "Tarball not found at $Tarball. Build it first: sudo ./build.sh wsl"
    exit 1
}

# Check that WSL is available.
if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
    Write-Error "WSL is not installed. Run 'wsl --install' first."
    exit 1
}

# Check whether the distro is already registered.
$existing = wsl.exe --list --quiet 2>$null | Where-Object { $_ -eq $DistroName }
if ($existing) {
    Write-Host "Distro '$DistroName' already exists. Unregister it first:" -ForegroundColor Yellow
    Write-Host "  wsl --unregister $DistroName" -ForegroundColor Yellow
    exit 1
}

if (-not (Test-Path $InstallPath)) {
    New-Item -ItemType Directory -Force -Path $InstallPath | Out-Null
}

Write-Host ":: importing $DistroName from $Tarball -> $InstallPath"
wsl.exe --import $DistroName $InstallPath $Tarball --version 2

Write-Host ":: setting WSL version to 2"
wsl.exe --set-version $DistroName 2 2>$null

Write-Host ""
Write-Host "Done. Launch with: wsl -d $DistroName" -ForegroundColor Green
Write-Host "First boot may take a few seconds while systemd starts." -ForegroundColor Green

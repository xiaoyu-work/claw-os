# install.ps1 — Helper to install claw-os into WSL2 on Windows.
#
# Usage (run in PowerShell):
#   .\install.ps1                                            # default settings
#   .\install.ps1 -DistroName claw-os-dev                    # custom name
#   .\install.ps1 -InstallPath D:\WSL\claw-os                # custom location
#   .\install.ps1 -Package .\claw-os-wsl-amd64.wsl           # explicit package
#
# By default the package name is selected from the host CPU architecture
# ($env:PROCESSOR_ARCHITECTURE → amd64 or arm64) so the same script
# works on Windows-on-ARM (Surface Pro X, Snapdragon X, Apple Silicon
# Mac via Parallels) without changes.
#
# Requirements:
#   - Windows 10 21H2+ or Windows 11
#   - WSL2 enabled (`wsl --install` if not yet)

[CmdletBinding()]
param(
    [string]$DistroName  = "claw-os",
    [string]$InstallPath = "$env:LOCALAPPDATA\WSL\claw-os",
    [string]$Package     = ""
)

$ErrorActionPreference = "Stop"

if (-not $Package) {
    $archSuffix = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
    $Package    = "$PSScriptRoot\..\..\build\claw-os-wsl-$archSuffix.wsl"
}

if (-not (Test-Path $Package -PathType Leaf)) {
    Write-Error "WSL package not found at $Package. Build it first: sudo ./build.sh wsl"
    exit 1
}
$Package = (Resolve-Path $Package).Path

# Check that WSL is available.
if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
    Write-Error "WSL is not installed. Run 'wsl --install' first."
    exit 1
}
$wslHelp = (& wsl.exe --help 2>&1 | Out-String) -replace "`0", ""
if ($wslHelp -notmatch "--from-file") {
    Write-Error "Claw OS requires WSL 2.4.4 or newer. Run 'wsl --update' first."
    exit 1
}

# Check whether the distro is already registered.
$registered = ((& wsl.exe --list --quiet 2>$null | Out-String) -replace "`0", "") `
    -split "\r?\n" | ForEach-Object { $_.Trim() } | Where-Object { $_ }
if ($registered -contains $DistroName) {
    Write-Host "Distro '$DistroName' already exists. Unregister it first:" -ForegroundColor Yellow
    Write-Host "  wsl --unregister $DistroName" -ForegroundColor Yellow
    exit 1
}

if (Test-Path $InstallPath) {
    if (Get-ChildItem -Force $InstallPath | Select-Object -First 1) {
        Write-Error "Install path is not empty: $InstallPath"
        exit 1
    }
} else {
    $installParent = Split-Path -Parent $InstallPath
    if ($installParent) {
        New-Item -ItemType Directory -Force -Path $installParent | Out-Null
    }
}

Write-Host ":: installing $DistroName from $Package -> $InstallPath"
& wsl.exe --install --from-file $Package --name $DistroName --location $InstallPath --version 2
if ($LASTEXITCODE -ne 0) {
    throw "WSL installation failed with exit code $LASTEXITCODE"
}

Write-Host ""
Write-Host "Done. Launch with: wsl -d $DistroName" -ForegroundColor Green
Write-Host "The first launch asks you to create the UNIX account." -ForegroundColor Green

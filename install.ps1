# NotMux Windows Installer
# Usage: irm https://raw.githubusercontent.com/contember/vryn-ws/main/install.ps1 | iex
# Or: .\install.ps1 [-Version "1.0.0"]

param(
    [string]$Version
)

$ErrorActionPreference = "Stop"

$Repo = "contember/vryn-ws"
$InstallDir = "$env:LOCALAPPDATA\Programs\NotMux"
$BinName = "notmux.exe"

# Get version
if (-not $Version) {
    Write-Host "Fetching latest version..."
    try {
        $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $release.tag_name -replace '^v', ''
    } catch {
        Write-Error "Failed to fetch latest version. Specify version with -Version parameter."
        exit 1
    }
}

Write-Host "Installing NotMux v$Version..."

# Download
$Artifact = "notmux-windows-x64"
$DownloadUrl = "https://github.com/$Repo/releases/download/v$Version/$Artifact.zip"
$TempDir = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_ }
$ZipPath = Join-Path $TempDir "notmux.zip"

Write-Host "Downloading from $DownloadUrl..."
Invoke-WebRequest -Uri $DownloadUrl -OutFile $ZipPath -UseBasicParsing

# Extract
Write-Host "Extracting..."
Expand-Archive -Path $ZipPath -DestinationPath $TempDir -Force

# Find the extracted executable (may be in a subdirectory)
$ExePath = Get-ChildItem -Path $TempDir -Recurse -Filter $BinName | Select-Object -First 1
if (-not $ExePath) {
    Write-Error "Could not find $BinName in extracted archive."
    exit 1
}

# Install
Write-Host "Installing to $InstallDir..."
if (Test-Path $InstallDir) {
    Remove-Item -Recurse -Force $InstallDir
}
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Copy-Item -Path $ExePath.FullName -Destination $InstallDir

# Add to PATH (user scope)
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    Write-Host "Adding to PATH..."
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    $env:Path = "$env:Path;$InstallDir"
}

# Create Start Menu shortcut
$StartMenuDir = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
$ShortcutPath = Join-Path $StartMenuDir "NotMux.lnk"

Write-Host "Creating Start Menu shortcut..."
$WshShell = New-Object -ComObject WScript.Shell
$Shortcut = $WshShell.CreateShortcut($ShortcutPath)
$Shortcut.TargetPath = Join-Path $InstallDir $BinName
$Shortcut.WorkingDirectory = $InstallDir
$Shortcut.Description = "Terminal multiplexer for managing multiple terminal sessions"
$Shortcut.Save()

# Cleanup
Remove-Item -Recurse -Force $TempDir

Write-Host ""
Write-Host "NotMux installed successfully!" -ForegroundColor Green
Write-Host ""
Write-Host "  Location: $InstallDir\$BinName"
Write-Host "  Shortcut: $ShortcutPath"
Write-Host ""
Write-Host "Launch from Start Menu or run: notmux"
Write-Host ""
Write-Host "Note: You may need to restart your terminal for PATH changes to take effect."

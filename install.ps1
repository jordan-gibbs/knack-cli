# knack CLI installer for Windows (PowerShell 5.1+).
#
# Usage:
#   irm https://knack.ai/install.ps1 | iex
#   $env:KNACK_VERSION = '0.5.0'; irm https://knack.ai/install.ps1 | iex
#   $env:KNACK_BIN_DIR = 'C:\tools\knack'; irm https://knack.ai/install.ps1 | iex
#
# Detects arch, downloads the matching zip from GitHub Releases, extracts
# to %LOCALAPPDATA%\knack\bin (or $env:KNACK_BIN_DIR), and appends that
# path to the *user* PATH if missing. Idempotent.

[CmdletBinding()]
param(
    [string]$Version = $env:KNACK_VERSION,
    [string]$BinDir  = $env:KNACK_BIN_DIR,
    [string]$GhRepo  = $(if ($env:KNACK_GH_REPO) { $env:KNACK_GH_REPO } else { 'jordan-gibbs/knack-cli' })
)

$ErrorActionPreference = 'Stop'

if (-not $Version) { $Version = 'latest' }
if (-not $BinDir)  { $BinDir  = Join-Path $env:LOCALAPPDATA 'knack\bin' }

# Windows only ships x86_64 for v1.
if (-not [Environment]::Is64BitOperatingSystem) {
    Write-Error 'knack-install: 32-bit Windows is not supported.'
    exit 1
}
$target = 'x86_64-pc-windows-msvc'

# Resolve version via the GitHub releases API.
if ($Version -eq 'latest') {
    $api = "https://api.github.com/repos/$GhRepo/releases/latest"
    $resp = Invoke-WebRequest -UseBasicParsing -Uri $api -Headers @{ 'User-Agent' = 'knack-installer' }
    $json = $resp.Content | ConvertFrom-Json
    if (-not $json.tag_name) {
        Write-Error "knack-install: couldn't resolve latest release from $GhRepo"
        exit 1
    }
    $Version = $json.tag_name -replace '^v', ''
}

$archive = "knack-$target.zip"
$url = "https://github.com/$GhRepo/releases/download/v$Version/$archive"

Write-Host "-> knack v$Version for $target"
Write-Host "-> $url"

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("knack-install-{0}" -f ([guid]::NewGuid())))
try {
    $zipPath = Join-Path $tmp $archive
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zipPath
    Expand-Archive -LiteralPath $zipPath -DestinationPath $tmp -Force

    $bin = Get-ChildItem -Path $tmp -Filter 'knack.exe' -Recurse | Select-Object -First 1
    if (-not $bin) {
        Write-Error 'knack-install: knack.exe not found in archive.'
        exit 1
    }

    if (-not (Test-Path $BinDir)) { New-Item -ItemType Directory -Path $BinDir | Out-Null }
    Copy-Item -LiteralPath $bin.FullName -Destination (Join-Path $BinDir 'knack.exe') -Force

    Write-Host "[OK] installed to $BinDir\knack.exe"
} finally {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
}

# Add to user PATH idempotently. We *only* touch user PATH, never machine.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$BinDir*") {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $BinDir } else { "$BinDir;$userPath" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host ""
    Write-Host "Added $BinDir to user PATH. Restart your shell for changes to take effect."
}

# Verify: invoke the freshly installed binary directly so we don't depend
# on the new PATH being in this session yet.
$knackExe = Join-Path $BinDir 'knack.exe'
& $knackExe --version

# Register knack with the AI agent the user is running in (Claude Code,
# Codex, Cursor, ...). Best-effort.
try {
    & $knackExe install --auto | Out-Null
    Write-Host "[OK] registered with detected agent (re-run 'knack install' to refresh)"
} catch {
    Write-Host "  Tip: run 'knack install' to register the CLI with your AI agent."
}

Write-Host ""
Write-Host "Next: run 'knack init' to pick self-host (GitHub) or Knack Cloud."
Write-Host "To uninstall later: iwr https://knack.ai/uninstall.ps1 | iex"

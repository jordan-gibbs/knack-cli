# knack CLI uninstaller for Windows (PowerShell 5.1+).
#
# Usage:
#   iwr https://getknack.ai/uninstall.ps1 | iex
#
# Companion to install.ps1. Removes the binary, the user-PATH entry,
# the per-user ~/.knack cache directory, and (best-effort) the agent
# shims via `knack uninstall` if the binary is still present.
#
# Always run from a separate PowerShell from any running knack.exe so
# the file lock on the running executable doesn't block deletion.

[CmdletBinding()]
param(
    [string]$BinDir = $env:KNACK_BIN_DIR
)

$ErrorActionPreference = 'Continue'

if (-not $BinDir) { $BinDir = Join-Path $env:LOCALAPPDATA 'knack\bin' }
$KnackRoot = Split-Path -Parent $BinDir   # %LOCALAPPDATA%\knack
$KnackExe  = Join-Path $BinDir 'knack.exe'
$KnackHome = Join-Path $env:USERPROFILE '.knack'

# 1. Best-effort: ask the CLI to strip every detected agent config
#    block + shim file before we delete it. Swallow errors so a broken
#    binary or missing network still lets the rest of the cleanup run.
if (Test-Path $KnackExe) {
    try {
        & $KnackExe uninstall --yes --quiet --keep-auth 2>$null | Out-Null
    } catch {
        # Intentional: the binary may be broken; carry on with filesystem cleanup.
    }
}

# 2. Remove the binary directory and any sibling caches under %LOCALAPPDATA%\knack.
if (Test-Path $KnackRoot) {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $KnackRoot
    if (Test-Path $KnackRoot) {
        Write-Warning "Could not remove $KnackRoot. Close any shell still running knack.exe and try again."
    } else {
        Write-Host "[OK] removed $KnackRoot"
    }
}

# 3. Strip $BinDir from the user PATH. Only touch user PATH, never machine.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -and ($userPath -like "*$BinDir*")) {
    $entries = $userPath -split ';' | Where-Object { $_ -and ($_ -ne $BinDir) }
    $newPath = ($entries -join ';')
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "[OK] removed $BinDir from user PATH (restart your shell to refresh)"
}

# 4. Remove the per-user cache (~/.knack with auth.json, installed.json,
#    update-check.json). May already be gone if step 1 succeeded.
if (Test-Path $KnackHome) {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $KnackHome
    if (Test-Path $KnackHome) {
        Write-Warning "Could not remove $KnackHome. Delete it manually."
    } else {
        Write-Host "[OK] removed $KnackHome"
    }
}

Write-Host ""
Write-Host "knack removed. Open a new shell for PATH to refresh."

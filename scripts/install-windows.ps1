# kindboard Windows installer — prebuilt, checksum-verified, SmartScreen-neutral.
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1 [-Run | -Help]
#
#   (no args)  install kindboard to %USERPROFILE%\.local\bin\kindboard.exe
#   -Run       install (if needed), then launch kindboard in the foreground
#   -Help      show this help
#
# What it does:
#   1. resolves the release to install: KINDBOARD_VERSION env override
#      (e.g. v0.1.6), else the latest GitHub release via the releases API
#   2. downloads kindboard-windows-x86_64.zip + SHA256SUMS for that tag
#      (TLS 1.2 enforced)
#   3. verifies the zip sha256 against SHA256SUMS (Get-FileHash)
#   4. expands the zip to a temp dir and moves kindboard.exe into
#      %USERPROFILE%\.local\bin (user-scoped, no admin)
#   5. clears the Mark-of-the-Web Zone.Identifier alternate stream (step 6
#      below); then, under -Run, launches kindboard in the foreground
#
# Why the MOTW clear (the whole point of this installer): SmartScreen only
# evaluates files carrying the Mark-of-the-Web (Zone.Identifier ADS). The exe
# reached this machine over HTTPS through this trusted, checksum-verified
# installer, so clearing MOTW at install time means the first launch shows NO
# SmartScreen prompt — analogous to the macOS quarantine clear in
# scripts/install-macos.sh. No code-signing certificate (EV or otherwise) is
# needed.
#
# Idempotent: an existing binary already reporting the resolved version is
# left alone (and is launched directly under -Run, so offline reuse works).
# Zero elevation: NO RunAs, NO #Requires -RunAsAdministrator, no registry
# writes, no PATH mutation — everything is user-scoped. The PATH hint at the
# end is printed only, never applied.
#
# Env:
#   KINDBOARD_VERSION=vX.Y.Z  pin a specific release (default: latest)
#   KINDBOARD_INSTALL_DIR     install dir (default: %USERPROFILE%\.local\bin;
#                             test/override knob)
#
# Log prefix: `install-windows:`.

[CmdletBinding()]
param(
    [switch]$Run,
    [switch]$Help
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Log { Write-Host "install-windows: $($args -join ' ')" }
function Fail {
    Write-Host "install-windows: ERROR: $($args -join ' ')" -ForegroundColor Red
    exit 1
}

if ($Help) {
@"
Usage: powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1 [-Run | -Help]
  (no args)  install kindboard to %USERPROFILE%\.local\bin\kindboard.exe
  -Run       install (if needed), then launch kindboard in the foreground
  -Help      show this help
"@
    exit 0
}

# ---------------------------------------------------------------------------
# 1. Platform + arch guards
# ---------------------------------------------------------------------------

if ($env:OS -ne 'Windows_NT') {
    Fail 'run this on Windows — this installer fetches the prebuilt windows binary'
}
if (-not [Environment]::Is64BitOperatingSystem) {
    Fail 'only 64-bit Windows (x86_64) is supported — arm64 is not shipped yet'
}

$installDir = $env:KINDBOARD_INSTALL_DIR
if (-not $installDir) { $installDir = Join-Path $env:USERPROFILE '.local\bin' }
$dest = Join-Path $installDir 'kindboard.exe'

# ---------------------------------------------------------------------------
# 2. Release version
# ---------------------------------------------------------------------------

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$tag = $env:KINDBOARD_VERSION
if (-not $tag) {
    Log 'no KINDBOARD_VERSION set — resolving the latest release via the GitHub API'
    try {
        $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/Orpere/kindboard/releases/latest' -UseBasicParsing
        $tag = $release.tag_name
    } catch {
        Fail 'could not resolve the latest release — set KINDBOARD_VERSION=vX.Y.Z and retry (offline?)'
    }
    Log "latest release: $tag"
} elseif ($tag -notmatch '^v') {
    Fail "KINDBOARD_VERSION must look like vX.Y.Z (got '$tag')"
}
$vernum = $tag.TrimStart('v')
if (-not $vernum) { Fail "bad release tag '$tag'" }

# ---------------------------------------------------------------------------
# 3. Idempotency: skip when the installed binary already reports the version
# ---------------------------------------------------------------------------

if (Test-Path -LiteralPath $dest) {
    $verOut = ''
    try {
        $verOut = (& $dest --version 2>$null) -join ' '
    } catch {
        $verOut = ''
    }
    $m = [regex]::Match($verOut, '\d+\.\d+\.\d+')
    $verNum = if ($m.Success) { $m.Value } else { '' }
    if ($verNum -eq $vernum) {
        Log "already installed ($vernum) — nothing to do"
        if ($Run) {
            & $dest
            exit $LASTEXITCODE
        }
        exit 0
    }
    Log "existing $dest does not report $vernum (got: '$verOut') — reinstalling"
}

# ---------------------------------------------------------------------------
# 4. Download zip + checksums, verify, extract, install
# ---------------------------------------------------------------------------

$base = "https://github.com/Orpere/kindboard/releases/download/$tag"
$zipName = 'kindboard-windows-x86_64.zip'
$workDir = Join-Path ([IO.Path]::GetTempPath()) ("kindboard-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $workDir -Force | Out-Null
try {
    $zip = Join-Path $workDir $zipName
    $sums = Join-Path $workDir 'SHA256SUMS'

    Log "downloading $base/$zipName"
    Invoke-WebRequest -Uri "$base/$zipName" -OutFile $zip -UseBasicParsing
    Log "downloading $base/SHA256SUMS"
    Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing

    $match = Select-String -LiteralPath $sums -Pattern '^\s*([0-9a-f]{64})\s+.*kindboard-windows-x86_64\.zip' | Select-Object -First 1
    if (-not $match) {
        Fail "SHA256SUMS has no entry for $zipName — refusing to install"
    }
    $expected = $match.Matches[0].Groups[1].Value
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
    if ($actual -ne $expected) {
        Fail "checksum mismatch for $zipName (expected $expected, got $actual) — refusing to install"
    }
    Log "checksum verified: $actual"

    $extractDir = Join-Path $workDir 'extract'
    New-Item -ItemType Directory -Path $extractDir -Force | Out-Null
    Log "extracting $zipName"
    Expand-Archive -LiteralPath $zip -DestinationPath $extractDir
    $src = Join-Path $extractDir 'kindboard.exe'
    if (-not (Test-Path -LiteralPath $src)) {
        Fail "zip does not contain 'kindboard.exe' — refusing to install"
    }

    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Move-Item -LiteralPath $src -Destination $dest -Force
    Log "installed $dest ($vernum)"

    # Mark-of-the-Web clear: SmartScreen only evaluates files carrying the
    # Zone.Identifier ADS; the exe arrived through this trusted,
    # checksum-verified installer, so clearing MOTW at install time means the
    # first launch shows NO SmartScreen prompt — analogous to the macOS
    # quarantine clear in scripts/install-macos.sh; no cert/EV signing needed.
    Remove-Item -LiteralPath "$dest:Zone.Identifier" -ErrorAction SilentlyContinue
} finally {
    Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# 5. PATH hint (read-only — never edits the registry or user profile)
# ---------------------------------------------------------------------------

if (($env:Path -split ';') -notcontains $installDir) {
    Write-Host "install-windows: note: $installDir is not on your PATH — add it with: setx PATH `"$env:Path;$installDir`""
}

# ---------------------------------------------------------------------------
# 6. -Run: launch in the foreground
# ---------------------------------------------------------------------------

if ($Run) {
    & $dest
    exit $LASTEXITCODE
}

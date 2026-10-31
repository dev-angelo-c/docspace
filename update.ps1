# DocSpace updater for Windows PowerShell 5.1 / PowerShell 7.
#
# Looks in your Downloads folder for DocSpace releases named  docspace-app-vX.Y.Z.zip,
# installs the newest one over this folder if it is newer than what you have, and then
# deletes older DocSpace zips from Downloads, keeping only the newest two (current + previous).
#
#   powershell -ExecutionPolicy Bypass -File .\update.ps1              # update if a newer zip is there
#   powershell -ExecutionPolicy Bypass -File .\update.ps1 -Version 0.2.0   # install that exact version (also to roll back)
#   powershell -ExecutionPolicy Bypass -File .\update.ps1 -NoCleanup   # don't delete any old zips
#
# Never touched: your index and ratings (stored in %APPDATA%), and the compiled build cache
# (src-tauri\target), so the next start only recompiles what changed.

param(
    [string]$Version,
    [string]$Downloads,
    [switch]$NoCleanup,
    [switch]$Quiet
)

$ErrorActionPreference = 'Continue'
$AppDir = $PSScriptRoot
$KeepZips = 2

function Say($text, $color = 'Gray') { if (-not $Quiet) { Write-Host $text -ForegroundColor $color } }
function Fail($text) { Write-Host "Update stopped: $text" -ForegroundColor Red; exit 1 }

# --- Where are Downloads? (works when Downloads has been moved to another drive or OneDrive) ---
if (-not $Downloads) {
    try { $Downloads = (New-Object -ComObject Shell.Application).NameSpace('shell:Downloads').Self.Path } catch { }
    if (-not $Downloads -or -not (Test-Path $Downloads)) { $Downloads = Join-Path $env:USERPROFILE 'Downloads' }
}

# --- What's installed? ----------------------------------------------------------------------------
$confPath = Join-Path $AppDir 'src-tauri\tauri.conf.json'
if (-not (Test-Path $confPath)) { Fail "run this script from the docspace-app folder (no src-tauri\tauri.conf.json here)." }
$installed = [version]((Get-Content $confPath -Raw | ConvertFrom-Json).version)

# --- Which releases are in Downloads? -------------------------------------------------------------
# docspace-app-v0.2.0.zip, also "docspace-app-v0.2.0 (1).zip" from repeat downloads.
# Zips without a version in the name (the very first release) count as 0.0.0.
$releases = @(Get-ChildItem -Path $Downloads -Filter 'docspace-app*.zip' -File -ErrorAction SilentlyContinue |
    ForEach-Object {
        $v = [version]'0.0.0'
        if ($_.Name -match '^docspace-app-v(\d+\.\d+\.\d+)') { $v = [version]$Matches[1] }
        [pscustomobject]@{ File = $_; Version = $v }
    } | Sort-Object -Property @{ Expression = 'Version'; Descending = $true }, @{ Expression = { $_.File.LastWriteTime }; Descending = $true })

if ($releases.Count -eq 0) {
    Say "No DocSpace zips found in $Downloads. You have v$installed."
    exit 0
}

if ($Version) {
    $target = $releases | Where-Object { $_.Version -eq [version]$Version } | Select-Object -First 1
    if (-not $target) { Fail "docspace-app-v$Version.zip is not in $Downloads." }
} else {
    $target = $releases[0]
}

# --- Install --------------------------------------------------------------------------------------
if ($target.Version -eq $installed -and -not $Version) {
    Say "DocSpace v$installed is up to date." 'Green'
} elseif ($target.Version -lt $installed -and -not $Version) {
    Say "DocSpace v$installed is newer than anything in Downloads. Nothing to install." 'Green'
} else {
    Say "Installing DocSpace v$($target.Version) over v$installed from $($target.File.Name) ..." 'Cyan'
    $tmp = Join-Path $env:TEMP ("docspace-update-" + [guid]::NewGuid().ToString('N'))
    try {
        Expand-Archive -Path $target.File.FullName -DestinationPath $tmp -Force -ErrorAction Stop
    } catch {
        Fail "could not unzip $($target.File.Name): $($_.Exception.Message)"
    }
    # The zip holds a single docspace-app folder; find the folder that has src-tauri in it.
    $src = Get-ChildItem -Path $tmp -Recurse -Directory -Filter 'src-tauri' -ErrorAction SilentlyContinue |
        Select-Object -First 1 | ForEach-Object { $_.Parent.FullName }
    if (-not $src) { Remove-Item $tmp -Recurse -Force; Fail "$($target.File.Name) doesn't look like a DocSpace release." }
    Get-ChildItem -Path $src -Recurse -File | Unblock-File -ErrorAction SilentlyContinue

    # Mirror the release into this folder: new and changed files are copied, files the new
    # version no longer has are removed. The build cache and generated schemas are left alone.
    $null = & robocopy $src $AppDir /MIR /R:2 /W:1 /NFL /NDL /NJH /NJS /NP `
        /XD (Join-Path $AppDir 'src-tauri\target') (Join-Path $AppDir 'src-tauri\gen') `
        /XF 'build-log.txt'
    $rc = $LASTEXITCODE
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    if ($rc -ge 8) { Fail "copying files failed (robocopy code $rc). Close DocSpace and any editor using these files, then try again." }

    $now = [version]((Get-Content $confPath -Raw | ConvertFrom-Json).version)
    Say "Updated DocSpace: v$installed -> v$now" 'Green'
    $changelog = Join-Path $AppDir 'CHANGELOG.md'
    if ((Test-Path $changelog) -and -not $Quiet) {
        Write-Host ''
        Get-Content $changelog -TotalCount 25 | ForEach-Object { Write-Host "  $_" }
    }
}

# --- Clean up Downloads: keep the newest two DocSpace zips, delete the rest ------------------------
if (-not $NoCleanup) {
    $keepVersions = @($releases | Select-Object -ExpandProperty Version -Unique | Select-Object -First $KeepZips)
    $kept = @{}
    foreach ($r in $releases) {
        $key = $r.Version.ToString()
        if (($keepVersions -contains $r.Version) -and -not $kept.ContainsKey($key)) {
            $kept[$key] = $true      # newest file of a kept version stays
            continue
        }
        Remove-Item -LiteralPath $r.File.FullName -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path -LiteralPath $r.File.FullName)) { Say "Removed old download: $($r.File.Name)" }
    }
}
exit 0

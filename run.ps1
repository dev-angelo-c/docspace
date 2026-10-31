# Start DocSpace in development mode, or build a Windows installer with -Installer.
# Checks Downloads for a newer DocSpace release first (see update.ps1).
#
#   powershell -ExecutionPolicy Bypass -File .\run.ps1
#   powershell -ExecutionPolicy Bypass -File .\run.ps1 -Installer

param([switch]$Installer)

$ErrorActionPreference = 'Continue'
Set-Location -Path $PSScriptRoot

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ((Test-Path $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) { $env:Path = "$cargoBin;$env:Path" }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Rust is not installed yet. Run:  powershell -ExecutionPolicy Bypass -File .\setup.ps1'
}

# Install a newer DocSpace release from Downloads first, if there is one.
$updater = Join-Path $PSScriptRoot 'update.ps1'
if (Test-Path $updater) { & $updater }

if ($Installer) {
    & cargo tauri build
    if ($LASTEXITCODE -eq 0) {
        # Cargo builds into CARGO_TARGET_DIR when it's set (a build folder shared by projects).
        $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot 'src-tauri\target' }
        $out = Join-Path $targetDir 'release\bundle\nsis'
        Write-Host "Installer written to $out" -ForegroundColor Green
        Invoke-Item $out
    }
} else {
    & cargo tauri dev
}
exit $LASTEXITCODE

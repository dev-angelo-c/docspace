# docspace.ps1: the one DocSpace script (Windows PowerShell 5.1 / PowerShell 7).
#
# Double-click DocSpace.bat, or run from PowerShell:
#   powershell -ExecutionPolicy Bypass -File .\docspace.ps1            # build + run (development)
#   powershell -ExecutionPolicy Bypass -File .\docspace.ps1 -Release   # build the Windows installer
#   powershell -ExecutionPolicy Bypass -File .\docspace.ps1 -Setup     # (re)check prerequisites only
#   add -DryRun to any of these to only list what would be deleted
#
# Every run:
#   1. Installs missing prerequisites (C++ Build Tools, WebView2, Rust, Tauri CLI, Ollama models).
#   2. If Downloads holds a DocSpace zip newer than this copy, unzips it and continues from there.
#   3. Cleans up Downloads: keeps this version's zip and the most recent prior zip, deletes
#      older DocSpace zips, and deletes every other unzipped copy of DocSpace.
#   4. Builds and runs DocSpace using ONE shared build folder, C:\cargo-target\docspace,
#      so a new copy reuses the compiled cache instead of recompiling everything.
#
# Never touched: your index database, tags and settings (in %APPDATA%), and the build cache.

param(
    [switch]$Release,
    [switch]$Setup,
    [switch]$DryRun,
    [string]$Downloads
)

$ErrorActionPreference = 'Continue'   # native tools write progress to stderr; failures are checked explicitly
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$AppDir    = $PSScriptRoot
$TargetDir = 'C:\cargo-target\docspace'   # DocSpace's own build cache (other projects use their own folder)
$ConfPath  = Join-Path $AppDir 'src-tauri\tauri.conf.json'
if (-not (Test-Path $ConfPath)) { Write-Host 'Run this from the DocSpace folder (src-tauri\tauri.conf.json is missing).' -ForegroundColor Red; exit 1 }
$Current   = [version]((Get-Content $ConfPath -Raw | ConvertFrom-Json).version)

# ================================ Prerequisite installer (from the old setup.ps1) ===============
$EmbedModel = 'nomic-embed-text'
$ChatModel  = 'qwen3:8b'
$SetupTemp  = Join-Path $env:TEMP 'docspace-setup'

function Step($text) { Write-Host ''; Write-Host "==> $text" -ForegroundColor Cyan }
function Ok($text)   { Write-Host "    OK: $text" -ForegroundColor Green }
function Warn($text) { Write-Host "    $text" -ForegroundColor Yellow }

function Get-File($url, $name) {
    New-Item -ItemType Directory -Force -Path $SetupTemp | Out-Null
    $out = Join-Path $SetupTemp $name
    Write-Host "    Downloading $name ..."
    Invoke-WebRequest -Uri $url -OutFile $out -UseBasicParsing -ErrorAction Stop
    return $out
}

function Add-CargoToPath {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if ((Test-Path $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) {
        $env:Path = "$cargoBin;$env:Path"
    }
}


function Install-Prerequisites {
    # --- 1. Microsoft C++ Build Tools (Rust needs Microsoft's linker on Windows) -------------
    Step 'C++ Build Tools'
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $haveVc = $false
    if (Test-Path $vswhere) {
        $vcPath = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        $haveVc = [bool]$vcPath
    }
    if ($haveVc) {
        Ok 'already installed'
    } else {
        $vs = Get-File 'https://aka.ms/vs/17/release/vs_BuildTools.exe' 'vs_BuildTools.exe'
        Warn 'Installing (about 2-4 GB, 5-15 minutes). Approve the Windows admin prompt if it appears.'
        $p = Start-Process -ErrorAction Stop -FilePath $vs -Wait -PassThru -ArgumentList @(
            '--wait', '--passive', '--norestart',
            '--add', 'Microsoft.VisualStudio.Workload.VCTools', '--includeRecommended')
        # 3010 = success, restart required
        if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { throw "Build Tools installer failed (exit code $($p.ExitCode))." }
        if ($p.ExitCode -eq 3010) {
            Warn 'Build Tools installed, but Windows needs a RESTART before Rust can use them.'
            Warn 'Restart the computer, then run setup.ps1 again.'
            return $false
        }
        Ok 'installed'
    }
    # Verify the C++ tools really are there before compiling anything with Rust.
    $vcPath = $null
    if (Test-Path $vswhere) { $vcPath = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
    if (-not $vcPath) {
        Warn 'The C++ build tools (link.exe) are still missing. Fix: open "Visual Studio Installer" from the Start menu,'
        Warn 'click Modify on Build Tools, tick "Desktop development with C++", click Modify/Install, restart, then run setup.ps1 again.'
        return $false
    }

    # --- 2. WebView2 runtime (draws the app window; usually already on Windows 10) ------------
    Step 'WebView2 runtime'
    $wv2Key = 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $wv2Key32 = 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $haveWv2 = (Test-Path "HKLM:\$wv2Key") -or (Test-Path "HKLM:\$wv2Key32") -or (Test-Path "HKCU:\$wv2Key32")
    if ($haveWv2) {
        Ok 'already installed'
    } else {
        $wv = Get-File 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' 'MicrosoftEdgeWebview2Setup.exe'
        Start-Process -ErrorAction Stop -FilePath $wv -Wait -ArgumentList '/silent', '/install'
        Ok 'installed'
    }

    # --- 3. Rust (rustup, MSVC toolchain) ------------------------------------------------------
    Step 'Rust'
    Add-CargoToPath
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        & rustup default stable-msvc | Out-Null
        Ok (& rustc --version)
    } else {
        $ri = Get-File 'https://win.rustup.rs/x86_64' 'rustup-init.exe'
        $p = Start-Process -ErrorAction Stop -FilePath $ri -Wait -PassThru -NoNewWindow -ArgumentList @(
            '-y', '--default-toolchain', 'stable', '--default-host', 'x86_64-pc-windows-msvc', '--profile', 'minimal')
        if ($p.ExitCode -ne 0) { throw "rustup-init failed (exit code $($p.ExitCode))." }
        Add-CargoToPath
        Ok (& rustc --version)
    }

    # --- 4. Tauri CLI (`cargo tauri ...`) -------------------------------------------------------
    Step 'Tauri CLI'
    & cargo tauri --version 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Ok (& cargo tauri --version)
    } else {
        Warn 'Building the Tauri CLI from source (one time, about 3-6 minutes)...'
        & cargo install tauri-cli --version '^2' --locked
        if ($LASTEXITCODE -ne 0) { throw 'cargo install tauri-cli failed (see the messages above).' }
        Ok (& cargo tauri --version)
    }

    # --- 5. Ollama models ------------------------------------------------------------------------
    Step 'Ollama models'
    $ollama = Get-Command ollama -ErrorAction SilentlyContinue
    if (-not $ollama) {
        $guess = Join-Path $env:LOCALAPPDATA 'Programs\Ollama\ollama.exe'
        if (Test-Path $guess) { $ollama = $guess }
    }
    if (-not $ollama) {
        Warn 'Ollama was not found. Install it from https://ollama.com/download/windows, then run setup.ps1 again.'
    } else {
        $installed = (& $ollama list 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0) {
            Warn 'Ollama is installed but not running. Start it from the Start menu, then run setup.ps1 again.'
        } else {
            foreach ($m in @($EmbedModel, $ChatModel)) {
                $name = if ($m -like '*:*') { $m } else { "${m}:latest" }
                if ($installed -match [regex]::Escape($name)) {
                    Ok "$m already downloaded"
                } else {
                    Write-Host "    Downloading $m ..."
                    & $ollama pull $m
                    if ($LASTEXITCODE -ne 0) { throw "ollama pull $m failed." }
                    Ok "$m downloaded"
                }
            }
        }
    }

    return $true
}

function Say($text, $color = 'Gray') { Write-Host $text -ForegroundColor $color }

function Remove-Safely($path, $what) {
    if ($DryRun) { Say "  [dry run] would delete $what $path" 'Yellow'; return }
    Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $path) { Say "  Could not delete $path (close DocSpace or any window using it, then run again)." 'Yellow' }
    else { Say "  Deleted $what $path" }
}

# Is $dir an unzipped DocSpace copy? (checked by the app identifier, not just the folder name)
function Test-DocSpaceCopy($dir) {
    $conf = Join-Path $dir 'src-tauri\tauri.conf.json'
    if (-not (Test-Path -LiteralPath $conf)) { return $false }
    try { return ((Get-Content -LiteralPath $conf -Raw | ConvertFrom-Json).identifier -eq 'app.docspace.desktop') } catch { return $false }
}

# ================================ 1. Prerequisites ================================================
Add-CargoToPath
$needSetup = $Setup -or -not (Get-Command cargo -ErrorAction SilentlyContinue)
if (-not $needSetup) { & cargo tauri --version 2>&1 | Out-Null; $needSetup = ($LASTEXITCODE -ne 0) }
if ($needSetup) {
    if (-not (Install-Prerequisites)) { exit 1 }
    if ($Setup) { Say ''; Say 'Prerequisites are ready. Run DocSpace.bat to build and start DocSpace.' 'Green'; exit 0 }
}

# ================================ 2. Newer zip in Downloads? ======================================
if (-not $Downloads) {
    try { $Downloads = (New-Object -ComObject Shell.Application).NameSpace('shell:Downloads').Self.Path } catch { }
    if (-not $Downloads -or -not (Test-Path $Downloads)) { $Downloads = Join-Path $env:USERPROFILE 'Downloads' }
}

# docspace-app-v0.5.1.zip, also "docspace-app-v0.5.1 (1).zip" from repeat downloads.
$zips = @(Get-ChildItem -Path $Downloads -Filter 'docspace-app*.zip' -File -ErrorAction SilentlyContinue |
    ForEach-Object {
        $v = [version]'0.0.0'
        if ($_.Name -match '^docspace-app-v(\d+\.\d+\.\d+)') { $v = [version]$Matches[1] }
        [pscustomobject]@{ File = $_; Version = $v }
    } | Sort-Object @{ Expression = 'Version'; Descending = $true }, @{ Expression = { $_.File.LastWriteTime }; Descending = $true })

if ($zips.Count -gt 0 -and $zips[0].Version -gt $Current) {
    $new = $zips[0]
    $dest = Join-Path $Downloads ("docspace-app-v" + $new.Version)
    Say "Found newer DocSpace v$($new.Version) in Downloads (this copy is v$Current)." 'Cyan'
    if ($DryRun) { Say "  [dry run] would unzip it to $dest and continue from there." 'Yellow'; exit 0 }
    if (-not (Test-DocSpaceCopy $dest)) {
        try { Expand-Archive -Path $new.File.FullName -DestinationPath $dest -Force -ErrorAction Stop }
        catch { Say "Could not unzip $($new.File.Name): $($_.Exception.Message)" 'Red'; exit 1 }
    }
    $script = Get-ChildItem -Path $dest -Recurse -Filter 'docspace.ps1' -File | Select-Object -First 1
    if (-not $script) { Say "$($new.File.Name) doesn't contain docspace.ps1." 'Red'; exit 1 }
    Get-ChildItem -Path $dest -Recurse -File | Unblock-File -ErrorAction SilentlyContinue
    Set-Location $env:TEMP   # don't hold this old folder open, so the new copy can delete it
    & $script.FullName @PSBoundParameters
    exit $LASTEXITCODE
}

# ================================ 3. Clean up old copies ==========================================
Say "DocSpace v$Current  ($AppDir)" 'Cyan'
Say 'Cleaning up Downloads...'

# Zips: keep this version's newest zip and the newest zip of the most recent prior version.
$prior = $zips | Where-Object { $_.Version -lt $Current } | Select-Object -First 1
$keep = @()
$cur = $zips | Where-Object { $_.Version -eq $Current } | Select-Object -First 1
if ($cur)   { $keep += $cur.File.FullName }
if ($prior) { $keep += $prior.File.FullName }
foreach ($z in $zips) {
    if ($keep -notcontains $z.File.FullName) { Remove-Safely $z.File.FullName 'old zip' }
}

# Unzipped copies: every DocSpace folder in Downloads (top level or one level down) except this one.
$here = (Resolve-Path $AppDir).Path.TrimEnd('\')
$copies = @()
foreach ($d in Get-ChildItem -Path $Downloads -Directory -ErrorAction SilentlyContinue) {
    if (Test-DocSpaceCopy $d.FullName) { $copies += $d.FullName; continue }
    foreach ($sub in Get-ChildItem -Path $d.FullName -Directory -ErrorAction SilentlyContinue) {
        if (Test-DocSpaceCopy $sub.FullName) { $copies += $d.FullName; break }
    }
}

# First run with the shared cache: reuse the biggest existing build cache instead of recompiling.
if (-not (Test-Path (Join-Path $TargetDir 'debug')) -and -not $DryRun) {
    $old = @(@($here) + $copies | ForEach-Object { Join-Path $_ 'src-tauri\target' }) +
           @($copies | ForEach-Object { Get-ChildItem -Path $_ -Recurse -Directory -Filter 'target' -Depth 2 -ErrorAction SilentlyContinue | ForEach-Object FullName }) |
           Where-Object { Test-Path (Join-Path $_ 'debug') } | Select-Object -Unique
    if ($old) {
        $best = $old | Sort-Object { (Get-ChildItem $_ -Recurse -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum } -Descending | Select-Object -First 1
        Say "Moving the existing build cache to $TargetDir (one time) ..."
        New-Item -ItemType Directory -Force -Path (Split-Path $TargetDir) | Out-Null
        if (Test-Path $TargetDir) { Remove-Item $TargetDir -Recurse -Force -ErrorAction SilentlyContinue }
        Move-Item -LiteralPath $best -Destination $TargetDir -ErrorAction SilentlyContinue
    }
}

foreach ($c in $copies | Select-Object -Unique) {
    $full = (Resolve-Path $c).Path.TrimEnd('\')
    if ($here -eq $full -or $here.StartsWith($full + '\')) { continue }   # never delete the copy we're running from
    Remove-Safely $full 'old copy'
}
# This copy's own old in-folder cache (from versions before the shared build folder).
$ownOld = Join-Path $AppDir 'src-tauri\target'
if (Test-Path $ownOld) { Remove-Safely $ownOld 'old in-folder build cache' }

if ($DryRun) { Say 'Dry run finished. Nothing was deleted or built.' 'Green'; exit 0 }

# ================================ 4. Build and run ================================================
New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null
$env:CARGO_TARGET_DIR = $TargetDir      # this window only
Set-Location (Join-Path $AppDir 'src-tauri')
Say "Build cache: $env:CARGO_TARGET_DIR"

if ($Release) {
    & cargo tauri build
    if ($LASTEXITCODE -eq 0) {
        $out = Join-Path $TargetDir 'release\bundle\nsis'
        Say "Installer written to $out" 'Green'
        Invoke-Item $out
    }
} else {
    & cargo tauri dev
}
exit $LASTEXITCODE

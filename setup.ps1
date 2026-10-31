# DocSpace setup for Windows 10/11 (Windows PowerShell 5.1 or PowerShell 7).
# Installs whatever is missing: C++ Build Tools, WebView2, Rust, the Tauri CLI,
# and the two Ollama models. Safe to run again; finished steps are skipped.
#
# Run it from the docspace-app folder:
#   powershell -ExecutionPolicy Bypass -File .\setup.ps1

# 'Continue' because native tools (cargo, rustup) write progress to stderr, which
# Windows PowerShell 5.1 would otherwise treat as a fatal error. Failures are checked explicitly.
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'   # makes Invoke-WebRequest much faster on PS 5.1
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$EmbedModel = 'nomic-embed-text'
$ChatModel  = 'qwen3:8b'
$Downloads  = Join-Path $env:TEMP 'docspace-setup'
New-Item -ItemType Directory -Force -Path $Downloads | Out-Null

function Step($text) { Write-Host ''; Write-Host "==> $text" -ForegroundColor Cyan }
function Ok($text)   { Write-Host "    OK: $text" -ForegroundColor Green }
function Warn($text) { Write-Host "    $text" -ForegroundColor Yellow }

function Get-File($url, $name) {
    $out = Join-Path $Downloads $name
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
        exit 1
    }
    Ok 'installed'
}
# Verify the C++ tools really are there before compiling anything with Rust.
$vcPath = $null
if (Test-Path $vswhere) { $vcPath = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
if (-not $vcPath) {
    Warn 'The C++ build tools (link.exe) are still missing. Fix: open "Visual Studio Installer" from the Start menu,'
    Warn 'click Modify on Build Tools, tick "Desktop development with C++", click Modify/Install, restart, then run setup.ps1 again.'
    exit 1
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

Write-Host ''
Write-Host 'Setup finished. Start DocSpace with:' -ForegroundColor Green
Write-Host '    powershell -ExecutionPolicy Bypass -File .\run.ps1'
Write-Host '(The first start compiles the app and takes a few minutes.)'

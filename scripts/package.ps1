param(
    [string]$Configuration = "release",
    [string]$FfmpegPath = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$DistRoot = Join-Path $RepoRoot "dist"
$AppName = "VideoSniffer"
$PackageRoot = Join-Path $DistRoot $AppName
$AppBinDir = Join-Path $PackageRoot "bin"
$ExtensionSource = Join-Path $RepoRoot "extensions\chrome"
$ExtensionOutDir = Join-Path $DistRoot "browser-extension"
$ExtensionZip = Join-Path $ExtensionOutDir "VideoSniffer-Chrome-Extension.zip"

function Reset-Directory($Path) {
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Find-Ffmpeg($ExplicitPath) {
    if ($ExplicitPath -and (Test-Path -LiteralPath $ExplicitPath)) {
        return (Resolve-Path -LiteralPath $ExplicitPath).Path
    }

    $LocalFfmpeg = Join-Path $RepoRoot "third_party\ffmpeg\ffmpeg.exe"
    if (Test-Path -LiteralPath $LocalFfmpeg) {
        return (Resolve-Path -LiteralPath $LocalFfmpeg).Path
    }

    return $null
}

if (-not $SkipBuild) {
    cargo build --release
}

$ExistingPackagedExe = Join-Path $PackageRoot "$AppName.exe"
if (Test-Path -LiteralPath $ExistingPackagedExe) {
    $RunningPackagedApp = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $ExistingPackagedExe } |
        Select-Object -First 1

    if ($RunningPackagedApp) {
        throw "Packaged app is running. Close $ExistingPackagedExe before packaging again."
    }
}

Reset-Directory $PackageRoot
New-Item -ItemType Directory -Force -Path $AppBinDir | Out-Null

$ExePath = Join-Path $RepoRoot "target\$Configuration\video_sniffer.exe"
if (-not (Test-Path -LiteralPath $ExePath)) {
    throw "Built executable not found: $ExePath"
}

Copy-Item -LiteralPath $ExePath -Destination (Join-Path $PackageRoot "$AppName.exe") -Force

$ResolvedFfmpeg = Find-Ffmpeg $FfmpegPath
if ($ResolvedFfmpeg) {
    Copy-Item -LiteralPath $ResolvedFfmpeg -Destination (Join-Path $AppBinDir "ffmpeg.exe") -Force
    Write-Host "Bundled ffmpeg: $ResolvedFfmpeg"
} else {
    Write-Warning "ffmpeg.exe not found. HLS remux will fall back to system PATH or keep TS output."
}

Reset-Directory $ExtensionOutDir
$ExtensionStage = Join-Path $ExtensionOutDir "chrome"
Copy-Item -LiteralPath $ExtensionSource -Destination $ExtensionStage -Recurse -Force
if (Test-Path -LiteralPath $ExtensionZip) {
    Remove-Item -LiteralPath $ExtensionZip -Force
}
Compress-Archive -Path (Join-Path $ExtensionStage "*") -DestinationPath $ExtensionZip -Force

$ReadmePath = Join-Path $PackageRoot "README.txt"
@"
VideoSniffer

Run:
  VideoSniffer.exe

Browser extension:
  1. Open Chrome or Edge extensions page.
  2. Enable developer mode.
  3. Load unpacked extension from:
     $ExtensionStage
  Or distribute/install the zip:
     $ExtensionZip

FFmpeg:
  The package script copies ffmpeg to bin\ffmpeg.exe when available.
"@ | Set-Content -LiteralPath $ReadmePath -Encoding UTF8

Write-Host ""
Write-Host "Package complete:"
Write-Host "  App:       $PackageRoot"
Write-Host "  Extension: $ExtensionZip"

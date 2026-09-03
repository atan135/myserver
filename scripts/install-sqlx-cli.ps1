[CmdletBinding()]
param(
    [switch]$SkipHashCheck,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$ConfigPath = Join-Path $ProjectRoot 'db/config/sqlx-cli.json'
$BinPath = Join-Path $ProjectRoot 'bin'
$Binary = Join-Path $BinPath 'sqlx.exe'
$Cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$CargoBin = Join-Path $env:USERPROFILE '.cargo\bin\sqlx.exe'
$LogPath = Join-Path $env:TEMP ('sqlx-cli-install-' + (Get-Date).ToString('HHmmss') + '.log')

if (-not (Test-Path $ConfigPath)) {
  throw "sqlx-cli config not found: $ConfigPath"
}
if (-not (Test-Path $Cargo)) {
  throw "cargo.exe not found at $Cargo. Install Rust from https://rustup.rs/ first."
}

$config = Get-Content -Raw -Path $ConfigPath | ConvertFrom-Json
$version = $config.version
$winPlatform = $config.platforms.'win32-x64'
if (-not $winPlatform) {
  throw "db/config/sqlx-cli.json is missing platforms.win32-x64 entry."
}
$expectedHash = ($winPlatform.sha256 | Out-String).Trim().ToLower()
$expectedRustflags = $winPlatform.buildRustflags
if (-not $expectedRustflags) {
  throw "db/config/sqlx-cli.json is missing platforms.win32-x64.buildRustflags. The Windows artifact requires MSVC link.exe deterministic flags to be reproducible; refusing to install without them."
}

if (-not $Quiet) {
  Write-Host 'Installing sqlx-cli for MyServer' -ForegroundColor Cyan
  Write-Host ("  version       : " + $version)
  Write-Host ("  expected hash : " + $expectedHash)
  Write-Host ("  RUSTFLAGS     : " + $expectedRustflags)
  Write-Host ("  target binary : " + $Binary)
  Write-Host ("  install log   : " + $LogPath)
}

$env:CARGO_TERM_COLOR = 'never'
$env:RUSTFLAGS = $expectedRustflags
$env:CARGO_TERM_PROGRESS_WHEN = 'never'

if (-not $Quiet) { Write-Host ''; Write-Host '=== cargo install --force sqlx-cli ===' -ForegroundColor Cyan }
& $Cargo install --force --version $version --locked --no-default-features --features postgres,rustls sqlx-cli 2>&1 | Out-File -FilePath $LogPath -Encoding utf8
if ($LASTEXITCODE -ne 0) {
  Write-Host ("cargo install failed (exit " + $LASTEXITCODE + '). Tail of log:' ) -ForegroundColor Red
  Get-Content $LogPath -Tail 30
  throw "cargo install sqlx-cli v$version failed."
}

if (-not (Test-Path $CargoBin)) {
  throw "cargo install reported success but $CargoBin was not produced."
}

if (-not (Test-Path $BinPath)) {
  New-Item -ItemType Directory -Path $BinPath | Out-Null
}
if (Test-Path $Binary) { Remove-Item $Binary -Force }
Copy-Item $CargoBin $Binary

$actualHash = (Get-FileHash $Binary -Algorithm SHA256).Hash.ToLower()
$size = (Get-Item $Binary).Length

if (-not $Quiet) {
  Write-Host ''
  Write-Host '=== produced binary ===' -ForegroundColor Cyan
  Write-Host ("  path   : " + $Binary)
  Write-Host ("  size   : " + $size + ' bytes')
  Write-Host ("  hash   : " + $actualHash)
}

if ($actualHash -ne $expectedHash) {
  if ($SkipHashCheck) {
    Write-Warning ("SHA-256 mismatch but -SkipHashCheck was set. Expected " + $expectedHash + ' got ' + $actualHash + '.')
    Write-Warning 'After verifying the new hash on all expected toolchains, update db/config/sqlx-cli.json by hand before running dev-stack.'
  } else {
    Write-Host ''
    Write-Host 'SHA-256 of the newly built sqlx.exe does NOT match db/config/sqlx-cli.json' -ForegroundColor Red
    Write-Host ("  expected : " + $expectedHash)
    Write-Host ("  actual   : " + $actualHash)
    Write-Host ''
    Write-Host 'This usually means the local Rust/MSVC toolchain differs from the one that produced the registered hash.' -ForegroundColor Yellow
    Write-Host 'Verify that rustc/cargo and the MSVC toolchain are unchanged, then re-run with -SkipHashCheck and update db/config/sqlx-cli.json manually after confirming the new hash is stable across two rebuilds.' -ForegroundColor Yellow
    throw "sqlx.exe SHA-256 mismatch; refusing to leave the registry stale."
  }
}

if (-not $Quiet) {
  Write-Host ''
  Write-Host 'sqlx-cli installed and verified.' -ForegroundColor Green
  Write-Host ("  " + $Binary)
}

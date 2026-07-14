$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
$ServiceDir = Join-Path $ProjectRoot "apps\myforge-agent"

Write-Host "Starting myforge-agent" -ForegroundColor Cyan

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location $ServiceDir
try {
    if (-not (Test-Path -LiteralPath $cargo)) {
        Write-Error "cargo.exe not found at $cargo"
    }

    & $cargo build
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    $binary = Join-Path $ProjectRoot "target\debug\myforge-agent.exe"
    if (-not (Test-Path -LiteralPath $binary)) {
        Write-Error "myforge-agent.exe not found at $binary"
    }

    & $binary

    if ($LASTEXITCODE -eq -1073741510) {
        Write-Host "myforge-agent stopped by Ctrl+C"
        exit 0
    }

    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
} finally {
    Pop-Location
}

$ErrorActionPreference = "Stop"

$env:REGISTRY_ENABLED="true"

Write-Host "Starting game-proxy with service discovery enabled" -ForegroundColor Cyan

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location "$PSScriptRoot\..\apps\game-proxy"
try {
  if (Test-Path $cargo) {
    & $cargo build
    if ($LASTEXITCODE -ne 0) {
      exit $LASTEXITCODE
    }

    $binary = Join-Path (Get-Location) "target\debug\game-proxy.exe"
    if (-not (Test-Path $binary)) {
      Write-Error "game-proxy.exe not found at $binary"
    }

    & $binary

    if ($LASTEXITCODE -eq -1073741510) {
      Write-Host "game-proxy stopped by Ctrl+C"
      exit 0
    }

    if ($LASTEXITCODE -ne 0) {
      exit $LASTEXITCODE
    }
  } else {
    Write-Error "cargo.exe not found at $cargo"
  }
} finally {
  Pop-Location
}

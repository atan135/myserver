$ErrorActionPreference = "Stop"

$env:REGISTRY_ENABLED="true"
if (-not $env:SERVICE_INSTANCE_ID) {
  $env:SERVICE_INSTANCE_ID="chat-server-001"
}

Write-Host "Starting chat-server with service discovery enabled" -ForegroundColor Cyan
Write-Host "  InstanceId: $env:SERVICE_INSTANCE_ID" -ForegroundColor Gray

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location "$PSScriptRoot\..\apps\chat-server"
try {
  if (Test-Path $cargo) {
    & $cargo build
    if ($LASTEXITCODE -ne 0) {
      exit $LASTEXITCODE
    }

    $binary = Join-Path (Get-Location) "target\debug\chat-server.exe"
    if (-not (Test-Path $binary)) {
      Write-Error "chat-server.exe not found at $binary"
    }

    & $binary

    if ($LASTEXITCODE -eq -1073741510) {
      Write-Host "chat-server stopped by Ctrl+C"
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

param(
    [Parameter(Mandatory=$false)]
    [int]$Port = 9002,

    [Parameter(Mandatory=$false)]
    [string]$LogLevel = "info"
)

$ErrorActionPreference = "Stop"

$env:MATCH_BIND_ADDR="0.0.0.0:$Port"
$env:LOG_LEVEL="$LogLevel"
$env:REGISTRY_ENABLED="true"
if (-not $env:SERVICE_INSTANCE_ID) {
    $env:SERVICE_INSTANCE_ID="match-service-$Port"
}

Write-Host "Starting match-service" -ForegroundColor Cyan
Write-Host "  BindAddr: $env:MATCH_BIND_ADDR" -ForegroundColor Gray
Write-Host "  LogLevel: $LogLevel" -ForegroundColor Gray
Write-Host "  InstanceId: $env:SERVICE_INSTANCE_ID" -ForegroundColor Gray

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location "$PSScriptRoot\..\apps\match-service"
try {
  if (Test-Path $cargo) {
    & $cargo build
    if ($LASTEXITCODE -ne 0) {
      exit $LASTEXITCODE
    }

    $binary = Join-Path (Get-Location) "target\debug\match-service.exe"
    if (-not (Test-Path $binary)) {
      Write-Error "match-service.exe not found at $binary"
    }

    & $binary

    if ($LASTEXITCODE -eq -1073741510) {
      Write-Host "match-service stopped by Ctrl+C"
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

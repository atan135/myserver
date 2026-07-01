$ErrorActionPreference = "Stop"

if (-not $env:NATS_URL) {
    $env:NATS_URL = "nats://127.0.0.1:4222"
}
if (-not $env:REDIS_URL) {
    $env:REDIS_URL = "redis://127.0.0.1:6379"
}

$workspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
$workspaceNodeModules = Join-Path $workspaceRoot "node_modules"
$serviceDir = Join-Path $workspaceRoot "apps\metrics-collector"

Write-Host "Starting metrics-collector" -ForegroundColor Cyan
Write-Host "  NATS_URL: $env:NATS_URL" -ForegroundColor Gray
Write-Host "  REDIS_URL: $env:REDIS_URL" -ForegroundColor Gray

Push-Location $serviceDir
try {
    if (-not (Test-Path "node_modules") -and -not (Test-Path $workspaceNodeModules)) {
        Write-Host "Installing Node.js dependencies..." -ForegroundColor Yellow
        npm install
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }

    npm run dev

    if ($LASTEXITCODE -eq -1073741510) {
        Write-Host "metrics-collector stopped by Ctrl+C"
        exit 0
    }

    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
} finally {
    Pop-Location
}

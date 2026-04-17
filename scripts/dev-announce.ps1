param(
    [Parameter(Mandatory=$false)]
    [int]$Port = 9004,

    [Parameter(Mandatory=$false)]
    [Alias("Host")]
    [string]$ListenHost = "127.0.0.1",

    [Parameter(Mandatory=$false)]
    [string]$LogLevel = "info",

    [Parameter(Mandatory=$false)]
    [string]$InstanceId = "",

    [Parameter(Mandatory=$false)]
    [switch]$MysqlEnabled,

    [Parameter(Mandatory=$false)]
    [switch]$NoWatch
)

$ErrorActionPreference = "Stop"

$env:HOST = $ListenHost
$env:ANNOUNCE_PORT = "$Port"
$env:LOG_LEVEL = "$LogLevel"
$env:REGISTRY_ENABLED = "true"
$env:SERVICE_NAME = "announce-service"
$env:MYSQL_ENABLED = if ($MysqlEnabled) { "true" } else { "false" }

if ($InstanceId) {
    $env:SERVICE_INSTANCE_ID = $InstanceId
} elseif (-not $env:SERVICE_INSTANCE_ID) {
    $env:SERVICE_INSTANCE_ID = "announce-service-$Port"
}

$workspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$workspaceNodeModules = Join-Path $workspaceRoot "node_modules"
$serviceDir = Join-Path $workspaceRoot "apps\announce-service"
$runScript = if ($NoWatch) { "start" } else { "dev" }

Write-Host "Starting announce-service" -ForegroundColor Cyan
Write-Host "  Host: $ListenHost" -ForegroundColor Gray
Write-Host "  Port: $Port" -ForegroundColor Gray
Write-Host "  LogLevel: $LogLevel" -ForegroundColor Gray
Write-Host "  InstanceId: $env:SERVICE_INSTANCE_ID" -ForegroundColor Gray
Write-Host "  MySQL Enabled: $env:MYSQL_ENABLED" -ForegroundColor Gray
Write-Host "  Watch Mode: $(-not $NoWatch)" -ForegroundColor Gray

Push-Location $serviceDir
try {
    if (-not (Test-Path "node_modules") -and -not (Test-Path $workspaceNodeModules)) {
        Write-Host "Installing Node.js dependencies..." -ForegroundColor Yellow
        npm install
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }

    npm run $runScript

    if ($LASTEXITCODE -eq -1073741510) {
        Write-Host "announce-service stopped by Ctrl+C"
        exit 0
    }

    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
} finally {
    Pop-Location
}

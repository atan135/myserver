param(
    [Parameter(Mandatory=$false)]
    [string]$InstanceId = "game-server-001",

    [Parameter(Mandatory=$false)]
    [int]$Port = 7000,

    [Parameter(Mandatory=$false)]
    [Nullable[int]]$AdminPort = $null
)

$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path

$env:NODE_ENV="development"
$env:APP_ENV="local"
$env:REGISTRY_ENABLED="true"
$env:SERVICE_INSTANCE_ID=$InstanceId
$env:GAME_PORT=$Port

# Local socket file name; each instance needs a unique value.
$udsName = "myserver-$InstanceId.sock"
$env:GAME_LOCAL_SOCKET_NAME=$udsName

# Keep the default admin port aligned with apps/port.txt: game-server-admin=7500.
# For non-default game ports, use Port+1 unless AdminPort is explicitly passed.
if ($null -eq $AdminPort) {
    if ($Port -eq 7000) {
        $env:ADMIN_PORT = "7500"
    } else {
        $env:ADMIN_PORT = [string]($Port + 1)
    }
} else {
    $env:ADMIN_PORT = [string]$AdminPort
}

# Clean up a possible leftover local socket file in the project root.
$udsPath = Join-Path $ProjectRoot $udsName
if (Test-Path $udsPath) {
    Write-Host "Cleaning up leftover socket file: $udsPath" -ForegroundColor Yellow
    Remove-Item $udsPath -Force
}

Write-Host "Starting game-server instance: $InstanceId" -ForegroundColor Cyan
Write-Host "  Port: $Port" -ForegroundColor Gray
Write-Host "  AdminPort: $env:ADMIN_PORT" -ForegroundColor Gray
Write-Host "  UDS: $udsName" -ForegroundColor Gray

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location (Join-Path $ProjectRoot "apps\game-server")
try {
  if (Test-Path $cargo) {
    & $cargo build
    if ($LASTEXITCODE -ne 0) {
      exit $LASTEXITCODE
    }

    $binary = Join-Path $ProjectRoot "target\debug\game-server.exe"
    if (-not (Test-Path $binary)) {
      Write-Error "game-server.exe not found at $binary"
    }

    & $binary

    if ($LASTEXITCODE -eq -1073741510) {
      Write-Host "game-server stopped by Ctrl+C"
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

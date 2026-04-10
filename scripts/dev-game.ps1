param(
    [Parameter(Mandatory=$true)]
    [string]$InstanceId,

    [Parameter(Mandatory=$true)]
    [int]$Port,

    [Parameter(Mandatory=$false)]
    [int]$AdminPort = 0
)

$ErrorActionPreference = "Stop"

$env:REGISTRY_ENABLED="true"
$env:SERVICE_INSTANCE_ID=$InstanceId
$env:GAME_PORT=$Port

# UDS socket 文件名，每个实例需要不同
$udsName = "myserver-$InstanceId.sock"
$env:GAME_LOCAL_SOCKET_NAME=$udsName

# Admin 端口，如果没有指定则使用 Port+1
if ($AdminPort -eq 0) {
    $env:ADMIN_PORT=[string]($Port + 1)
} else {
    $env:ADMIN_PORT=[string]$AdminPort
}

# 清理可能残留的 UDS socket 文件（在项目根目录）
$projectRoot = "$PSScriptRoot\.."
$udsPath = Join-Path $projectRoot $udsName
if (Test-Path $udsPath) {
    Write-Host "Cleaning up leftover socket file: $udsPath" -ForegroundColor Yellow
    Remove-Item $udsPath -Force
}

Write-Host "Starting game-server instance: $InstanceId" -ForegroundColor Cyan
Write-Host "  Port: $Port" -ForegroundColor Gray
Write-Host "  AdminPort: $env:ADMIN_PORT" -ForegroundColor Gray
Write-Host "  UDS: $udsName" -ForegroundColor Gray

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location "$PSScriptRoot\..\apps\game-server"
try {
  if (Test-Path $cargo) {
    & $cargo build
    if ($LASTEXITCODE -ne 0) {
      exit $LASTEXITCODE
    }

    $binary = Join-Path (Get-Location) "target\debug\game-server.exe"
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

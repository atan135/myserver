param(
    [Parameter(Mandatory=$false)]
    [string]$InstanceId = "game-server-001",

    [Parameter(Mandatory=$false)]
    [int]$Port = 7000,

    [Parameter(Mandatory=$false)]
    [Nullable[int]]$AdminPort = $null
)

$ErrorActionPreference = "Stop"

$env:REGISTRY_ENABLED="true"
$env:SERVICE_INSTANCE_ID=$InstanceId
$env:GAME_PORT=$Port

# UDS socket 文件名，每个实例需要不同
$udsName = "myserver-$InstanceId.sock"
$env:GAME_LOCAL_SOCKET_NAME=$udsName

# Admin 端口默认与 apps/port.txt 保持一致：game-server-admin=7500
# 若指定了非默认 Port 且未显式传入 AdminPort，则回退到 Port+1 便于多实例本地调试
if ($null -eq $AdminPort) {
    if ($Port -eq 7000) {
        $env:ADMIN_PORT = "7500"
    } else {
        $env:ADMIN_PORT = [string]($Port + 1)
    }
} else {
    $env:ADMIN_PORT = [string]$AdminPort
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

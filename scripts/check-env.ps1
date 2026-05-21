$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ProjectBin = Join-Path $ProjectRoot "bin"

Write-Host "Checking Node.js"
node -v
npm -v

Write-Host "Checking protoc"
protoc --version

Write-Host "Checking MariaDB client"
mysql --version

Write-Host "Checking Rust absolute path"
$rustc = "$env:USERPROFILE\.cargo\bin\rustc.exe"
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"

if (Test-Path $rustc) {
  & $rustc -V
} else {
  Write-Warning "rustc.exe not found in default cargo directory"
}

if (Test-Path $cargo) {
  & $cargo -V
} else {
  Write-Warning "cargo.exe not found in default cargo directory"
}

Write-Host "Checking Redis absolute path"
$redisServer = Join-Path $ProjectBin "redis-server.exe"
if (-not (Test-Path $redisServer)) {
  $redisCommand = Get-Command redis-server -ErrorAction SilentlyContinue
  if ($redisCommand) {
    $redisServer = $redisCommand.Source
  } else {
    $redisServer = "C:\Program Files\Redis\redis-server.exe"
  }
}
if (Test-Path $redisServer) {
  & $redisServer --version
} else {
  Write-Warning "redis-server.exe not found in bin, PATH, or default path"
}

Write-Host "Checking NATS server"
$natsServer = Join-Path $ProjectBin "nats-server.exe"
if (-not (Test-Path $natsServer)) {
  $natsCommand = Get-Command nats-server -ErrorAction SilentlyContinue
  if ($natsCommand) {
    $natsServer = $natsCommand.Source
  }
}
if ($natsServer -and (Test-Path $natsServer)) {
  & $natsServer --version
} else {
  Write-Warning "nats-server.exe not found in bin or PATH"
}

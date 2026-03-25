$ErrorActionPreference = "Stop"

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
$redisServer = "C:\Program Files\Redis\redis-server.exe"
if (Test-Path $redisServer) {
  & $redisServer --version
} else {
  Write-Warning "redis-server.exe not found in default path"
}


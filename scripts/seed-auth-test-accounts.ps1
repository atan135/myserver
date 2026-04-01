# 传参执行
# powershell -ExecutionPolicy Bypass -File .\scripts\seed-auth-test-accounts.ps1 -Account test003 -Password Passw0rd! -DisplayName "Test User 003"
# 默认执行
# powershell -ExecutionPolicy Bypass -File .\scripts\seed-auth-test-accounts.ps1

param(
  [string]$File,
  [string]$Account,
  [string]$Password,
  [string]$DisplayName,
  [string]$Status = "active"
)

$ErrorActionPreference = "Stop"

$resolvedFile = $null
if ($File) {
  $resolvedFile = (Resolve-Path -LiteralPath $File).Path
}

$npmArgs = @("run", "seed:test-accounts")

if ($resolvedFile -or $Account -or $Password -or $DisplayName -or $Status -ne "active") {
  $npmArgs += "--"
}

if ($resolvedFile) {
  $npmArgs += @("--file", $resolvedFile)
}

if ($Account) {
  $npmArgs += @("--account", $Account)
}

if ($Password) {
  $npmArgs += @("--password", $Password)
}

if ($DisplayName) {
  $npmArgs += @("--display-name", $DisplayName)
}

if ($Status -ne "active") {
  $npmArgs += @("--status", $Status)
}

Push-Location "$PSScriptRoot\..\apps\auth-http"
try {
  & npm.cmd @npmArgs
} finally {
  Pop-Location
}

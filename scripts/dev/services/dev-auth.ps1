$ErrorActionPreference = "Stop"
$env:REGISTRY_ENABLED="true"

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path

Push-Location (Join-Path $ProjectRoot "apps\auth-http")
try {
  npm install
  npm run start
} finally {
  Pop-Location
}

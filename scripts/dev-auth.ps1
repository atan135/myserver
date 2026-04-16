$ErrorActionPreference = "Stop"
$env:REGISTRY_ENABLED="true"

Push-Location "$PSScriptRoot\..\apps\auth-http"
try {
  npm install
  npm run start
} finally {
  Pop-Location
}

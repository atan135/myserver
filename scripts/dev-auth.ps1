$ErrorActionPreference = "Stop"

Push-Location "$PSScriptRoot\..\apps\auth-http"
try {
  npm install
  npm run dev
} finally {
  Pop-Location
}


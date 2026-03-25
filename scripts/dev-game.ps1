$ErrorActionPreference = "Stop"

$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
Push-Location "$PSScriptRoot\..\apps\game-server"
try {
  if (Test-Path $cargo) {
    & $cargo run
  } else {
    Write-Error "cargo.exe not found at $cargo"
  }
} finally {
  Pop-Location
}


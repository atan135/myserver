param(
  [string]$Psql = "psql",
  [string]$User = "postgres",
  [switch]$Confirm
)

$ErrorActionPreference = "Stop"

if (-not $Confirm) {
  Write-Error "This script clears local MyServer development databases. Re-run with -Confirm to proceed."
}

$databases = @(
  "myserver_auth",
  "myserver_game",
  "myserver_chat",
  "myserver_announce",
  "myserver_mail"
)

foreach ($db in $databases) {
  Write-Host "Resetting $db..."
  & $Psql -U $User -d "postgres" -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS $db WITH (FORCE);"
}

& $Psql -U $User -f "db/init.sql"

Write-Host "Local MyServer development data reset complete."

param(
  [string]$Psql = "psql",
  [string]$User = "postgres",
  [ValidateSet("development")]
  [string]$Environment = "development",
  [switch]$Confirm
)

$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if (-not $Confirm) {
  throw "This script clears local MyServer development databases. Re-run with -Confirm to proceed."
}

if ($Environment -ne "development") {
  throw "reset-dev-data only supports the development environment."
}

$databaseConnections = @(
  @{ Database = "myserver_auth"; UrlName = "MYSERVER_DB_MIGRATION_AUTH_URL" },
  @{ Database = "myserver_game"; UrlName = "MYSERVER_DB_MIGRATION_GAME_URL" },
  @{ Database = "myserver_chat"; UrlName = "MYSERVER_DB_MIGRATION_CHAT_URL" },
  @{ Database = "myserver_announce"; UrlName = "MYSERVER_DB_MIGRATION_ANNOUNCE_URL" },
  @{ Database = "myserver_mail"; UrlName = "MYSERVER_DB_MIGRATION_MAIL_URL" }
)
$bootstrapHost = $null
$bootstrapPort = $null
foreach ($connection in $databaseConnections) {
  $value = [Environment]::GetEnvironmentVariable($connection.UrlName)
  if (-not $value) { throw "$($connection.UrlName) is required for the migration-based reset." }
  try { $uri = [Uri]$value } catch { throw "$($connection.UrlName) must be a PostgreSQL URL." }
  if ($uri.Scheme -notin @("postgres", "postgresql")) {
    throw "$($connection.UrlName) must use postgres:// or postgresql://."
  }
  if ($uri.Host -notin @("localhost", "127.0.0.1", "::1")) {
    throw "$($connection.UrlName) must target localhost; this reset cannot run against a shared environment."
  }
  if ($uri.AbsolutePath.Trim("/") -ne $connection.Database) {
    throw "$($connection.UrlName) must target $($connection.Database)."
  }
  $port = if ($uri.Port -gt 0) { $uri.Port } else { 5432 }
  if ($null -eq $bootstrapHost) {
    $bootstrapHost = $uri.Host
    $bootstrapPort = $port
  } elseif ($uri.Host -ne $bootstrapHost -or $port -ne $bootstrapPort) {
    throw "All migration URLs must target the same local PostgreSQL endpoint."
  }
}

$psqlConnectionArguments = @("--host", $bootstrapHost, "--port", $bootstrapPort, "--username", $User)
foreach ($connection in $databaseConnections) {
  $database = $connection.Database
  Write-Host "Resetting $database..."
  & $Psql @psqlConnectionArguments --dbname "postgres" -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS $database WITH (FORCE);"
  if ($LASTEXITCODE -ne 0) { throw "Failed to drop $database." }
}

& $Psql @psqlConnectionArguments --dbname "postgres" -v ON_ERROR_STOP=1 -f (Join-Path $ProjectRoot "db/bootstrap/development.sql")
if ($LASTEXITCODE -ne 0) { throw "Database bootstrap failed." }

& node (Join-Path $ProjectRoot "tools/db.js") up --database all --actor "local-reset"
if ($LASTEXITCODE -ne 0) { throw "Versioned migration failed. The reset leaves empty development databases for diagnosis." }

& $Psql @psqlConnectionArguments --dbname "myserver_auth" -v ON_ERROR_STOP=1 -f (Join-Path $ProjectRoot "db/seeds/development/auth-local-world.sql")
if ($LASTEXITCODE -ne 0) { throw "Development seed failed." }

Write-Host "Local MyServer development data reset complete."

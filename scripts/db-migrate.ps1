param(
    [switch]$Check,
    [switch]$DryRun,
    [switch]$List,
    [string]$MysqlUrl = $env:MYSQL_URL,
    [string]$EnvPath = ".env"
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $RepoRoot

function Read-EnvValue {
    param(
        [string]$Path,
        [string]$Name
    )

    if (-not (Test-Path $Path)) {
        return ""
    }

    foreach ($line in Get-Content $Path) {
        $trimmed = $line.Trim()
        if ($trimmed.Length -eq 0 -or $trimmed.StartsWith("#")) {
            continue
        }
        $parts = $trimmed.Split("=", 2)
        if ($parts.Length -eq 2 -and $parts[0].Trim() -eq $Name) {
            return $parts[1].Trim().Trim('"').Trim("'")
        }
    }

    return ""
}

$argsList = @()
if ($Check) {
    $argsList += "--check"
} elseif ($DryRun) {
    $argsList += "--dry-run"
} elseif ($List) {
    $argsList += "--list"
}

if (-not $MysqlUrl) {
    $MysqlUrl = Read-EnvValue -Path $EnvPath -Name "MYSQL_URL"
}

if ($MysqlUrl) {
    $argsList += "--mysql-url=$MysqlUrl"
}

node tools/db-migrate.js @argsList

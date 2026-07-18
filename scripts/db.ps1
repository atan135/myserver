param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("status", "up", "validate", "baseline", "drift", "backfill-status", "backfill-run", "backfill-pause", "backfill-resume")]
  [string]$Command,
  [Parameter(Mandatory = $true)]
  [ValidateSet("auth", "game", "chat", "announce", "mail", "all")]
  [string]$Database,
  [string]$Actor,
  [string]$ExpectedFingerprint,
  [string]$Environment,
  [string]$Task,
  [int]$MaxBatches
)

$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$arguments = @((Join-Path $ProjectRoot "tools/db.js"), $Command, "--database", $Database)
if ($Actor) {
  $arguments += @("--actor", $Actor)
}
if ($ExpectedFingerprint) {
  $arguments += @("--expected-fingerprint", $ExpectedFingerprint)
}
if ($Environment) {
  $arguments += @("--environment", $Environment)
}
if ($Task) {
  $arguments += @("--task", $Task)
}
if ($MaxBatches) {
  $arguments += @("--max-batches", $MaxBatches)
}

& node @arguments
exit $LASTEXITCODE

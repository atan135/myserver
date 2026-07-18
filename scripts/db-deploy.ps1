param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("validate", "preflight", "apply", "postflight", "rebuild-check")]
  [string]$Command,
  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[a-z][a-z0-9-]{0,63}$")]
  [string]$Environment,
  [string]$Actor,
  [switch]$CheckReadiness,
  [switch]$RequireReadiness,
  [switch]$ConfirmTemporaryRebuild
)

$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if ($Command -eq "apply" -and -not $Actor) {
  throw "apply requires -Actor for migration audit events."
}
if ($RequireReadiness -and -not $CheckReadiness) {
  throw "-RequireReadiness requires -CheckReadiness."
}
if ($Command -eq "rebuild-check" -and -not $ConfirmTemporaryRebuild) {
  throw "rebuild-check requires -ConfirmTemporaryRebuild and the MYSERVER_DB_DEPLOY_TEMPORARY_REBUILD=1 environment guard."
}
if ($Command -ne "rebuild-check" -and $ConfirmTemporaryRebuild) {
  throw "-ConfirmTemporaryRebuild is only valid for rebuild-check."
}

$arguments = @((Join-Path $ProjectRoot "tools/db-deploy.js"), $Command, "--environment", $Environment)
if ($Actor) {
  $arguments += @("--actor", $Actor)
}
if ($CheckReadiness) {
  $arguments += "--check-readiness"
}
if ($RequireReadiness) {
  $arguments += "--require-readiness"
}
if ($ConfirmTemporaryRebuild) {
  $arguments += @("--confirm-temporary-rebuild", "stage6-temporary-rebuild")
}

& node @arguments
exit $LASTEXITCODE

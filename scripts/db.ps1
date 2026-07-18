param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("status", "up", "validate", "baseline")]
  [string]$Command,
  [Parameter(Mandatory = $true)]
  [ValidateSet("auth", "game", "chat", "announce", "mail", "all")]
  [string]$Database,
  [string]$Actor,
  [string]$ExpectedFingerprint
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

& node @arguments
exit $LASTEXITCODE

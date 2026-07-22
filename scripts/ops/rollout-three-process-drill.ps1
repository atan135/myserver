[CmdletBinding()]
param(
    [ValidateSet("local", "test", "staging", "production")]
    [string]$EnvironmentName = "local",

    [string]$AdminApiUrl = $(if ($env:ADMIN_API_URL) { $env:ADMIN_API_URL } else { "http://127.0.0.1:3001" }),
    [string]$AdminApiToken = $env:ADMIN_API_TOKEN,

    [Parameter(Mandatory = $true)]
    [string]$WorldId,
    [Parameter(Mandatory = $true)]
    [string]$RolloutEpoch,
    [Parameter(Mandatory = $true)]
    [string]$RoomId,
    [Parameter(Mandatory = $true)]
    [string]$OldServerId,
    [Parameter(Mandatory = $true)]
    [string]$NewServerId,
    [Parameter(Mandatory = $true)]
    [string]$ProxyInstanceId,
    [string]$BackupReference,
    [string]$Reason = "controlled room transfer drill",
    [string]$RequestId = "room-transfer-$([Guid]::NewGuid().ToString())",
    [switch]$ExecuteSteps,
    [string]$ReportPath = ""
)

$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$ControlPlaneCli = Join-Path $ProjectRoot "tools\rollout\rollout-control-plane-cli.js"

function Write-Report([object]$Report) {
    if ([string]::IsNullOrWhiteSpace($ReportPath)) {
        return
    }
    $directory = Split-Path -Parent $ReportPath
    if (-not [string]::IsNullOrWhiteSpace($directory)) {
        New-Item -ItemType Directory -Force -Path $directory | Out-Null
    }
    $Report | ConvertTo-Json -Depth 20 | Set-Content -Path $ReportPath -Encoding utf8
}

if ($ExecuteSteps) {
    if ([string]::IsNullOrWhiteSpace($AdminApiToken)) {
        throw "ADMIN_API_TOKEN is required for -ExecuteSteps. Use an authorized admin-api JWT."
    }
    if (-not $PSBoundParameters.ContainsKey("BackupReference") -or [string]::IsNullOrWhiteSpace($BackupReference)) {
        throw "BackupReference must be explicitly provided for -ExecuteSteps."
    }
}

$arguments = @(
    $ControlPlaneCli,
    "--admin-api-url", $AdminApiUrl,
    "--world-id", $WorldId,
    "--rollout-epoch", $RolloutEpoch,
    "--room-id", $RoomId,
    "--old-server-id", $OldServerId,
    "--new-server-id", $NewServerId,
    "--proxy-instance-id", $ProxyInstanceId,
    "--backup-reference", $(if ([string]::IsNullOrWhiteSpace($BackupReference)) { "preview-backup-reference" } else { $BackupReference }),
    "--request-id", $RequestId,
    "--reason", $Reason
)

if ($ExecuteSteps) {
    $arguments += @("--admin-api-token", $AdminApiToken, "--execute")
} else {
    $arguments += "--dry-run"
}

Write-Host "Room Transfer control-plane drill: $(if ($ExecuteSteps) { "execute" } else { "dry-run" })" -ForegroundColor Cyan
Write-Host "Target: world=$WorldId room=$RoomId old=$OldServerId new=$NewServerId proxy=$ProxyInstanceId" -ForegroundColor Gray

$output = & node @arguments 2>&1
$exitCode = $LASTEXITCODE
$outputText = ($output | Out-String).Trim()
try {
    $transfer = $outputText | ConvertFrom-Json
} catch {
    throw "control-plane CLI did not return JSON: $outputText"
}

$report = [ordered]@{
    ok = ($exitCode -eq 0 -and $transfer.ok -eq $true)
    mode = $(if ($ExecuteSteps) { "execute" } else { "dry-run" })
    inputs = [ordered]@{
        environmentName = $EnvironmentName
        adminApiUrl = $AdminApiUrl
        worldId = $WorldId
        rolloutEpoch = $RolloutEpoch
        roomId = $RoomId
        oldServerId = $OldServerId
        newServerId = $NewServerId
        proxyInstanceId = $ProxyInstanceId
        requestId = $RequestId
    }
    safety = [ordered]@{
        startsServices = $false
        callsControlPlane = [bool]$ExecuteSteps
        requestsShutdown = $false
        runsReconnectClient = $false
    }
    transfer = $transfer
}
Write-Report $report

Write-Output ($report | ConvertTo-Json -Depth 20)
if (-not $report.ok) {
    exit 1
}

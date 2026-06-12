<#
.SYNOPSIS
Preflight and step runner for a first-stage old/new/proxy rollout drill.

.DESCRIPTION
This script does not start Redis, auth-http, game-server, or game-proxy.
By default it runs in dry-run mode: it checks local tools, probes the expected
ports, prints the manual service preparation commands, and prints the rollout
steps that would be executed.

Use -ExecuteSteps only after the old game-server, new game-server, game-proxy,
and auth-http are already running and configured for this drill. The shutdown
safety-gate request is still skipped unless -AllowShutdownRequest is passed.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1

Print preflight results and all drill commands without changing services.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 `
  -ExecuteSteps `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001 `
  -OldServerId game-server-old `
  -NewServerId game-server-new

Execute the rollout/drain/transfer/complete steps against already-running
services, but do not request old game-server shutdown.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/rollout-three-process-drill.ps1 `
  -ExecuteSteps `
  -AllowShutdownRequest `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001

Also call the old-server shutdown safety gate after complete-if-drained.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory=$false)]
    [switch]$ExecuteSteps,

    [Parameter(Mandatory=$false)]
    [switch]$AllowShutdownRequest,

    [Parameter(Mandatory=$false)]
    [switch]$SkipShutdownRequest,

    [Parameter(Mandatory=$false)]
    [switch]$SkipPortProbe,

    [Parameter(Mandatory=$false)]
    [string]$RoomId = $(if ($env:ROOM_ID) { $env:ROOM_ID } elseif ($env:MYSERVER_ROLLOUT_ROOM_ID) { $env:MYSERVER_ROLLOUT_ROOM_ID } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$RolloutEpoch = $(if ($env:ROLLOUT_EPOCH) { $env:ROLLOUT_EPOCH } elseif ($env:MYSERVER_ROLLOUT_EPOCH) { $env:MYSERVER_ROLLOUT_EPOCH } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$OldServerId = $(if ($env:MYSERVER_OLD_SERVER_ID) { $env:MYSERVER_OLD_SERVER_ID } else { "game-server-old" }),

    [Parameter(Mandatory=$false)]
    [string]$NewServerId = $(if ($env:MYSERVER_NEW_SERVER_ID) { $env:MYSERVER_NEW_SERVER_ID } else { "game-server-new" }),

    [Parameter(Mandatory=$false)]
    [int]$OldGamePort = $(if ($env:MYSERVER_OLD_GAME_PORT) { [int]$env:MYSERVER_OLD_GAME_PORT } else { 7000 }),

    [Parameter(Mandatory=$false)]
    [int]$NewGamePort = $(if ($env:MYSERVER_NEW_GAME_PORT) { [int]$env:MYSERVER_NEW_GAME_PORT } else { 7001 }),

    [Parameter(Mandatory=$false)]
    [string]$OldAdminHost = $(if ($env:MYSERVER_OLD_GAME_ADMIN_HOST) { $env:MYSERVER_OLD_GAME_ADMIN_HOST } else { "127.0.0.1" }),

    [Parameter(Mandatory=$false)]
    [int]$OldAdminPort = $(if ($env:MYSERVER_OLD_GAME_ADMIN_PORT) { [int]$env:MYSERVER_OLD_GAME_ADMIN_PORT } else { 7500 }),

    [Parameter(Mandatory=$false)]
    [string]$OldAdminToken = $(if ($env:MYSERVER_OLD_GAME_ADMIN_TOKEN) { $env:MYSERVER_OLD_GAME_ADMIN_TOKEN } elseif ($env:GAME_ADMIN_TOKEN) { $env:GAME_ADMIN_TOKEN } else { "dev-only-change-this-game-admin-token" }),

    [Parameter(Mandatory=$false)]
    [string]$NewAdminHost = $(if ($env:MYSERVER_NEW_GAME_ADMIN_HOST) { $env:MYSERVER_NEW_GAME_ADMIN_HOST } else { "127.0.0.1" }),

    [Parameter(Mandatory=$false)]
    [int]$NewAdminPort = $(if ($env:MYSERVER_NEW_GAME_ADMIN_PORT) { [int]$env:MYSERVER_NEW_GAME_ADMIN_PORT } else { 7501 }),

    [Parameter(Mandatory=$false)]
    [string]$NewAdminToken = $(if ($env:MYSERVER_NEW_GAME_ADMIN_TOKEN) { $env:MYSERVER_NEW_GAME_ADMIN_TOKEN } elseif ($env:GAME_ADMIN_TOKEN) { $env:GAME_ADMIN_TOKEN } else { "dev-only-change-this-game-admin-token" }),

    [Parameter(Mandatory=$false)]
    [string]$AuthBaseUrl = $(if ($env:MYSERVER_AUTH_BASE_URL) { $env:MYSERVER_AUTH_BASE_URL } else { "http://127.0.0.1:3000" }),

    [Parameter(Mandatory=$false)]
    [string]$ServiceToken = $(if ($env:MYSERVER_INTERNAL_API_TOKEN) { $env:MYSERVER_INTERNAL_API_TOKEN } elseif ($env:INTERNAL_API_TOKEN) { $env:INTERNAL_API_TOKEN } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$ProxyAdminUrl = $(if ($env:MYSERVER_PROXY_ADMIN_URL) { $env:MYSERVER_PROXY_ADMIN_URL } else { "http://127.0.0.1:7101" }),

    [Parameter(Mandatory=$false)]
    [string]$ProxyAdminToken = $(if ($env:PROXY_ADMIN_TOKEN) { $env:PROXY_ADMIN_TOKEN } else { "dev-only-change-this-proxy-admin-token" }),

    [Parameter(Mandatory=$false)]
    [int]$TimeoutMs = $(if ($env:MYSERVER_ROLLOUT_TIMEOUT_MS) { [int]$env:MYSERVER_ROLLOUT_TIMEOUT_MS } else { 5000 }),

    [Parameter(Mandatory=$false)]
    [string]$AdminActor = $(if ($env:MYSERVER_ADMIN_ACTOR) { $env:MYSERVER_ADMIN_ACTOR } else { "rollout-three-process-drill" })
)

$ErrorActionPreference = "Stop"

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$TransferCli = Join-Path $ProjectRoot "tools\mock-client\src\rollout-transfer-cli.js"
$MockClientIndex = Join-Path $ProjectRoot "tools\mock-client\src\index.js"
$script:StageResults = @()

function Write-Section {
    param([Parameter(Mandatory=$true)][string]$Title)
    Write-Host ""
    Write-Host "== $Title ==" -ForegroundColor Cyan
}

function Add-StageResult {
    param(
        [Parameter(Mandatory=$true)][string]$Stage,
        [Parameter(Mandatory=$true)][string]$Status,
        [Parameter(Mandatory=$false)][string]$Detail = ""
    )

    $script:StageResults += [pscustomobject]@{
        stage = $Stage
        status = $Status
        detail = $Detail
    }
}

function Format-CommandPart {
    param([Parameter(Mandatory=$true)][AllowEmptyString()][string]$Value)

    if ($Value -eq "") {
        return "''"
    }

    if ($Value -match "^[A-Za-z0-9_.,:/=@%+\\<>-]+$") {
        return $Value
    }

    return "'$($Value -replace "'", "''")'"
}

function Write-CommandLine {
    param([Parameter(Mandatory=$true)][string[]]$Parts)

    $line = ($Parts | ForEach-Object { Format-CommandPart $_ }) -join " "
    Write-Host "  $line" -ForegroundColor Gray
}

function Mask-TokenState {
    param([Parameter(Mandatory=$false)][AllowEmptyString()][string]$Token)

    if ([string]::IsNullOrWhiteSpace($Token)) {
        return "missing"
    }
    if ($Token -like "dev-only-change-this-*") {
        return "default-dev"
    }
    return "set"
}

function Join-UrlPath {
    param(
        [Parameter(Mandatory=$true)][string]$BaseUrl,
        [Parameter(Mandatory=$true)][string]$PathAndQuery
    )

    $base = $BaseUrl.TrimEnd("/")
    if ($PathAndQuery.StartsWith("/")) {
        return "$base$PathAndQuery"
    }
    return "$base/$PathAndQuery"
}

function Escape-QueryValue {
    param([Parameter(Mandatory=$true)][AllowEmptyString()][string]$Value)
    return [uri]::EscapeDataString($Value)
}

function Get-UriEndpoint {
    param([Parameter(Mandatory=$true)][string]$Url)

    $uri = [Uri]$Url
    $port = $uri.Port
    if ($port -le 0) {
        if ($uri.Scheme -eq "https") {
            $port = 443
        } else {
            $port = 80
        }
    }

    return [pscustomobject]@{
        host = $uri.Host
        port = [int]$port
    }
}

function Test-TcpPort {
    param(
        [Parameter(Mandatory=$true)][string]$HostName,
        [Parameter(Mandatory=$true)][int]$Port,
        [Parameter(Mandatory=$false)][int]$TimeoutMs = 500
    )

    $client = [System.Net.Sockets.TcpClient]::new()
    try {
        $connect = $client.BeginConnect($HostName, $Port, $null, $null)
        if (-not $connect.AsyncWaitHandle.WaitOne($TimeoutMs, $false)) {
            return $false
        }
        $client.EndConnect($connect)
        return $true
    } catch {
        return $false
    } finally {
        $client.Close()
    }
}

function Invoke-JsonPost {
    param(
        [Parameter(Mandatory=$true)][string]$Name,
        [Parameter(Mandatory=$true)][string]$Uri,
        [Parameter(Mandatory=$true)][hashtable]$Headers,
        [Parameter(Mandatory=$false)]$BodyObject = $null
    )

    Write-Host "Running $Name" -ForegroundColor Yellow
    $params = @{
        Method = "Post"
        Uri = $Uri
        Headers = $Headers
    }
    if ($null -ne $BodyObject) {
        $params.ContentType = "application/json"
        $params.Body = ($BodyObject | ConvertTo-Json -Compress -Depth 10)
    }

    $result = Invoke-RestMethod @params
    if ($null -ne $result) {
        Write-Host ($result | ConvertTo-Json -Depth 20)
    }
    return $result
}

function Invoke-ExternalStep {
    param(
        [Parameter(Mandatory=$true)][string]$Name,
        [Parameter(Mandatory=$true)][string]$FilePath,
        [Parameter(Mandatory=$true)][string[]]$Arguments
    )

    Write-Host "Running $Name" -ForegroundColor Yellow
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

function New-ProxyHeaders {
    return @{
        Authorization = "Bearer $ProxyAdminToken"
        "X-Admin-Actor" = $AdminActor
    }
}

function New-InternalHeaders {
    $headers = @{}
    if (-not [string]::IsNullOrWhiteSpace($ServiceToken)) {
        $headers["X-Service-Token"] = $ServiceToken
    }
    return $headers
}

function Get-MockClientServiceTokenArgs {
    if ([string]::IsNullOrWhiteSpace($ServiceToken)) {
        return @()
    }
    return @("--service-token", $ServiceToken)
}

function Write-RunSummary {
    Write-Section "Summary"
    if ($script:StageResults.Count -eq 0) {
        Write-Host "No stages were recorded." -ForegroundColor Gray
        return
    }

    foreach ($item in $script:StageResults) {
        $detail = if ($item.detail) { " - $($item.detail)" } else { "" }
        Write-Host ("{0,-34} {1}{2}" -f $item.stage, $item.status, $detail)
    }
}

$displayRoomId = if ($RoomId) { $RoomId } else { "<ROOM_ID>" }
$displayRolloutEpoch = if ($RolloutEpoch) { $RolloutEpoch } else { "<ROLLOUT_EPOCH>" }

Write-Section "Mode"
if ($ExecuteSteps) {
    Write-Host "EXECUTE mode: control endpoints will be called. Services must already be running." -ForegroundColor Yellow
} else {
    Write-Host "DRY-RUN mode: no service writes, no service starts, no integration stack execution." -ForegroundColor Green
}

Write-Host "ProjectRoot: $ProjectRoot" -ForegroundColor Gray
Write-Host "RoomId: $displayRoomId" -ForegroundColor Gray
Write-Host "RolloutEpoch: $displayRolloutEpoch" -ForegroundColor Gray
Write-Host "OldServerId: $OldServerId" -ForegroundColor Gray
Write-Host "NewServerId: $NewServerId" -ForegroundColor Gray

Write-Section "Preflight"
$preflightErrors = @()
$preflightWarnings = @()

if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    $preflightErrors += "node is not available on PATH"
} else {
    Write-Host "node: found" -ForegroundColor Green
}

if (-not (Test-Path $TransferCli)) {
    $preflightErrors += "missing $TransferCli"
} else {
    Write-Host "transfer cli: $TransferCli" -ForegroundColor Green
}

if (-not (Test-Path $MockClientIndex)) {
    $preflightErrors += "missing $MockClientIndex"
} else {
    Write-Host "mock-client index: $MockClientIndex" -ForegroundColor Green
}

if ($ExecuteSteps -and [string]::IsNullOrWhiteSpace($RoomId)) {
    $preflightErrors += "RoomId is required in -ExecuteSteps mode"
}

if ($ExecuteSteps -and [string]::IsNullOrWhiteSpace($RolloutEpoch)) {
    $preflightErrors += "RolloutEpoch is required in -ExecuteSteps mode"
}

Write-Host "Token states:" -ForegroundColor Gray
Write-Host "  old admin: $(Mask-TokenState $OldAdminToken)" -ForegroundColor Gray
Write-Host "  new admin: $(Mask-TokenState $NewAdminToken)" -ForegroundColor Gray
Write-Host "  proxy admin: $(Mask-TokenState $ProxyAdminToken)" -ForegroundColor Gray
Write-Host "  auth internal service token: $(Mask-TokenState $ServiceToken)" -ForegroundColor Gray

if (-not $SkipPortProbe) {
    $authEndpoint = Get-UriEndpoint $AuthBaseUrl
    $proxyEndpoint = Get-UriEndpoint $ProxyAdminUrl
    $probeItems = @(
        [pscustomobject]@{ name = "auth-http"; host = $authEndpoint.host; port = $authEndpoint.port },
        [pscustomobject]@{ name = "old game-server admin"; host = $OldAdminHost; port = $OldAdminPort },
        [pscustomobject]@{ name = "new game-server admin"; host = $NewAdminHost; port = $NewAdminPort },
        [pscustomobject]@{ name = "game-proxy admin"; host = $proxyEndpoint.host; port = $proxyEndpoint.port }
    )

    foreach ($probe in $probeItems) {
        $listening = Test-TcpPort -HostName $probe.host -Port $probe.port
        if ($listening) {
            Write-Host ("{0,-24} {1}:{2} listening" -f $probe.name, $probe.host, $probe.port) -ForegroundColor Green
        } else {
            $message = ("{0} is not listening on {1}:{2}" -f $probe.name, $probe.host, $probe.port)
            if ($ExecuteSteps) {
                $preflightErrors += $message
            } else {
                $preflightWarnings += $message
                Write-Warning $message
            }
        }
    }
} else {
    Write-Host "port probes skipped" -ForegroundColor Yellow
}

if ($preflightWarnings.Count -gt 0) {
    Add-StageResult "preflight" "warning" ($preflightWarnings -join "; ")
} else {
    Add-StageResult "preflight" "ok"
}

if ($preflightErrors.Count -gt 0) {
    foreach ($errorMessage in $preflightErrors) {
        Write-Error $errorMessage -ErrorAction Continue
    }
    Add-StageResult "preflight-gate" "failed" ($preflightErrors -join "; ")
    Write-RunSummary
    throw "Preflight failed"
}

Write-Section "Stage 0 - Manual Service Preparation"
Write-Host "This script never starts services. If needed, start dependencies and these processes in separate terminals." -ForegroundColor Yellow
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev-auth.ps1")
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev-game.ps1", "-InstanceId", $OldServerId, "-Port", [string]$OldGamePort, "-AdminPort", [string]$OldAdminPort)
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev-game.ps1", "-InstanceId", $NewServerId, "-Port", [string]$NewGamePort, "-AdminPort", [string]$NewAdminPort)
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev-proxy.ps1")
Write-Host "Prerequisite: auth-http internal game-server admin client must point to the old game-server for drain status and shutdown checks." -ForegroundColor Yellow
Add-StageResult "manual-service-preparation" "printed"

Write-Section "Stage 1 - Start Proxy Rollout"
$rolloutStartPath = "/rollout/start?rollout_epoch=$(Escape-QueryValue $displayRolloutEpoch)&old_server_id=$(Escape-QueryValue $OldServerId)&new_server_id=$(Escape-QueryValue $NewServerId)"
$rolloutStartUri = Join-UrlPath $ProxyAdminUrl $rolloutStartPath
Write-Host "POST $rolloutStartUri" -ForegroundColor Gray
Write-Host "  Authorization: Bearer <proxy-admin-token>" -ForegroundColor Gray
if ($ExecuteSteps) {
    Invoke-JsonPost -Name "proxy rollout start" -Uri $rolloutStartUri -Headers (New-ProxyHeaders) | Out-Null
    Add-StageResult "proxy-rollout-start" "ok"
} else {
    Add-StageResult "proxy-rollout-start" "planned"
}

Write-Section "Stage 2 - Enable Old Server Drain"
$configUri = Join-UrlPath $AuthBaseUrl "/api/v1/internal/game-server/config"
Write-Host "POST $configUri body { key=drain_mode_reason, value=rollout-drill:$displayRolloutEpoch }" -ForegroundColor Gray
Write-Host "POST $configUri body { key=drain_mode_source, value=scripts/rollout-three-process-drill.ps1 }" -ForegroundColor Gray
Write-Host "POST $configUri body { key=drain_mode, value=on }" -ForegroundColor Gray
if ($ExecuteSteps) {
    $internalHeaders = New-InternalHeaders
    Invoke-JsonPost -Name "old drain reason" -Uri $configUri -Headers $internalHeaders -BodyObject @{ key = "drain_mode_reason"; value = "rollout-drill:$RolloutEpoch" } | Out-Null
    Invoke-JsonPost -Name "old drain source" -Uri $configUri -Headers $internalHeaders -BodyObject @{ key = "drain_mode_source"; value = "scripts/rollout-three-process-drill.ps1" } | Out-Null
    Invoke-JsonPost -Name "old drain on" -Uri $configUri -Headers $internalHeaders -BodyObject @{ key = "drain_mode"; value = "on" } | Out-Null
    Add-StageResult "old-drain-enable" "ok"
} else {
    Add-StageResult "old-drain-enable" "planned"
}

Write-Section "Stage 3 - Select Transferable Room"
Write-Host "Use an already existing room on the old game-server with online_member_count == 0." -ForegroundColor Yellow
Write-Host "Online rooms are intentionally unsupported in this phase; freeze returns ROOM_TRANSFER_HAS_ONLINE_MEMBERS." -ForegroundColor Yellow
Write-Host "Useful discovery command:" -ForegroundColor Gray
$drainStatusDisplayArgs = @(
    "node",
    "tools/mock-client/src/index.js",
    "--scenario",
    "rollout-drain-status",
    "--http-base-url",
    $AuthBaseUrl,
    "--timeout-ms",
    [string]$TimeoutMs
)
if (-not [string]::IsNullOrWhiteSpace($ServiceToken)) {
    $drainStatusDisplayArgs += @("--service-token", "<service-token>")
}
Write-CommandLine $drainStatusDisplayArgs
Add-StageResult "room-selection-guidance" "printed" "room=$displayRoomId"

Write-Section "Stage 4 - Transfer Freeze/Export/Import/Confirm/Route/Retire"
$transferArgs = @(
    $TransferCli,
    "--rollout-epoch", $RolloutEpoch,
    "--room-id", $RoomId,
    "--old-server-id", $OldServerId,
    "--new-server-id", $NewServerId,
    "--old-admin-host", $OldAdminHost,
    "--old-admin-port", [string]$OldAdminPort,
    "--old-admin-token", $OldAdminToken,
    "--new-admin-host", $NewAdminHost,
    "--new-admin-port", [string]$NewAdminPort,
    "--new-admin-token", $NewAdminToken,
    "--proxy-admin-url", $ProxyAdminUrl,
    "--proxy-admin-token", $ProxyAdminToken,
    "--timeout-ms", [string]$TimeoutMs
)
$transferDryRunArgs = @(
    $TransferCli,
    "--dry-run",
    "--rollout-epoch", $displayRolloutEpoch,
    "--room-id", $displayRoomId,
    "--old-server-id", $OldServerId,
    "--new-server-id", $NewServerId,
    "--old-admin-host", $OldAdminHost,
    "--old-admin-port", [string]$OldAdminPort,
    "--old-admin-token", $OldAdminToken,
    "--new-admin-host", $NewAdminHost,
    "--new-admin-port", [string]$NewAdminPort,
    "--new-admin-token", $NewAdminToken,
    "--proxy-admin-url", $ProxyAdminUrl,
    "--proxy-admin-token", $ProxyAdminToken,
    "--timeout-ms", [string]$TimeoutMs
)
$transferDisplayArgs = @(
    "node",
    "tools/mock-client/src/rollout-transfer-cli.js",
    "--rollout-epoch", $displayRolloutEpoch,
    "--room-id", $displayRoomId,
    "--old-server-id", $OldServerId,
    "--new-server-id", $NewServerId,
    "--old-admin-host", $OldAdminHost,
    "--old-admin-port", [string]$OldAdminPort,
    "--old-admin-token", "<old-admin-token>",
    "--new-admin-host", $NewAdminHost,
    "--new-admin-port", [string]$NewAdminPort,
    "--new-admin-token", "<new-admin-token>",
    "--proxy-admin-url", $ProxyAdminUrl,
    "--proxy-admin-token", "<proxy-admin-token>",
    "--timeout-ms", [string]$TimeoutMs
)
Write-CommandLine $transferDisplayArgs
if ($ExecuteSteps) {
    Invoke-ExternalStep -Name "room transfer orchestration" -FilePath "node" -Arguments $transferArgs
    Add-StageResult "room-transfer" "ok"
} else {
    Write-Host "Transfer dry-run plan:" -ForegroundColor Gray
    & node @transferDryRunArgs
    if ($LASTEXITCODE -ne 0) {
        Add-StageResult "room-transfer-dry-run" "failed" "rollout-transfer-cli validation failed"
        Write-RunSummary
        throw "room transfer dry-run plan failed with exit code $LASTEXITCODE"
    }
    Add-StageResult "room-transfer-dry-run" "ok"
    Add-StageResult "room-transfer" "planned"
}

Write-Section "Stage 5 - Query Old Drain Status"
$drainStatusArgs = @(
    $MockClientIndex,
    "--scenario", "rollout-drain-status",
    "--http-base-url", $AuthBaseUrl,
    "--timeout-ms", [string]$TimeoutMs
) + (Get-MockClientServiceTokenArgs)
Write-CommandLine $drainStatusDisplayArgs
if ($ExecuteSteps) {
    Invoke-ExternalStep -Name "old rollout drain status" -FilePath "node" -Arguments $drainStatusArgs
    Add-StageResult "old-drain-status" "ok"
} else {
    Add-StageResult "old-drain-status" "planned"
}

Write-Section "Stage 6 - Complete Proxy Rollout If Drained"
$completeUri = Join-UrlPath $ProxyAdminUrl "/rollout/complete-if-drained"
Write-Host "POST $completeUri" -ForegroundColor Gray
Write-Host "  Authorization: Bearer <proxy-admin-token>" -ForegroundColor Gray
if ($ExecuteSteps) {
    Invoke-JsonPost -Name "proxy complete-if-drained" -Uri $completeUri -Headers (New-ProxyHeaders) | Out-Null
    Add-StageResult "proxy-complete-if-drained" "ok"
} else {
    Add-StageResult "proxy-complete-if-drained" "planned"
}

Write-Section "Stage 7 - Optional Shutdown Safety Gate"
$shutdownDisplayArgs = @(
    "node",
    "tools/mock-client/src/index.js",
    "--scenario",
    "request-server-shutdown",
    "--http-base-url",
    $AuthBaseUrl,
    "--shutdown-reason",
    "rollout-three-process-drill:$displayRolloutEpoch",
    "--timeout-ms",
    [string]$TimeoutMs
)
if (-not [string]::IsNullOrWhiteSpace($ServiceToken)) {
    $shutdownDisplayArgs += @("--service-token", "<service-token>")
}
Write-CommandLine $shutdownDisplayArgs

if ($SkipShutdownRequest) {
    Write-Host "Shutdown request skipped by -SkipShutdownRequest." -ForegroundColor Yellow
    Add-StageResult "shutdown-safety-gate" "skipped" "SkipShutdownRequest"
} elseif (-not $AllowShutdownRequest) {
    Write-Host "Shutdown request is not executed unless -AllowShutdownRequest is passed." -ForegroundColor Yellow
    Add-StageResult "shutdown-safety-gate" "skipped" "requires AllowShutdownRequest"
} elseif ($ExecuteSteps) {
    $shutdownArgs = @(
        $MockClientIndex,
        "--scenario", "request-server-shutdown",
        "--http-base-url", $AuthBaseUrl,
        "--shutdown-reason", "rollout-three-process-drill:$RolloutEpoch",
        "--timeout-ms", [string]$TimeoutMs
    ) + (Get-MockClientServiceTokenArgs)
    Invoke-ExternalStep -Name "old server shutdown safety gate" -FilePath "node" -Arguments $shutdownArgs
    Add-StageResult "shutdown-safety-gate" "ok"
} else {
    Add-StageResult "shutdown-safety-gate" "planned" "requires ExecuteSteps and AllowShutdownRequest"
}

Write-RunSummary

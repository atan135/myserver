<#
.SYNOPSIS
Preflight and step runner for a first-stage old/new/proxy rollout drill.

.DESCRIPTION
This script does not start Redis, auth-http, game-server, or game-proxy.
By default it runs in dry-run mode: it checks local tools, resolves control
endpoints through service registry discovery, probes the resolved endpoints,
prints the manual local service preparation commands, and prints the rollout
steps that would be executed.

Use -ExecuteSteps only after the old game-server, new game-server, game-proxy,
and auth-http are already running, registered, and configured for this drill.
Fixed 127.0.0.1 ports are only a local/manual fallback when registry discovery
is explicitly non-required; strict, test, and production paths must discover
auth-http, game-proxy admin, and game-server admin endpoints from the registry.
The shutdown safety-gate request is still skipped unless -AllowShutdownRequest
is passed.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/ops/rollout-three-process-drill.ps1

Print preflight results and all drill commands without changing services.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/ops/rollout-three-process-drill.ps1 `
  -ExecuteSteps `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001 `
  -OldServerId game-server-old `
  -NewServerId game-server-new

Execute the rollout/drain/transfer/complete steps against already-running
services, but do not request old game-server shutdown.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts/ops/rollout-three-process-drill.ps1 `
  -ExecuteSteps `
  -AllowShutdownRequest `
  -OldProcessPidFile .tmp\rollout-drill-pids.json `
  -RolloutEpoch rollout-20260612-a `
  -RoomId room-empty-001

Also call the old-server shutdown safety gate after complete-if-drained and
wait for the old game-server process recorded in the PID file to exit.
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
    [string]$ProxyInstanceId = $(if ($env:MYSERVER_PROXY_INSTANCE_ID) { $env:MYSERVER_PROXY_INSTANCE_ID } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$AuthInstanceId = $(if ($env:MYSERVER_AUTH_INSTANCE_ID) { $env:MYSERVER_AUTH_INSTANCE_ID } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$EnvironmentName = $(if ($env:MYSERVER_ENVIRONMENT_NAME) { $env:MYSERVER_ENVIRONMENT_NAME } elseif ($env:APP_ENV) { $env:APP_ENV } elseif ($env:NODE_ENV) { $env:NODE_ENV } else { "local" }),

    [Parameter(Mandatory=$false)]
    [object]$RegistryEnabled = $(if ($env:REGISTRY_ENABLED) { $env:REGISTRY_ENABLED } else { $true }),

    [Parameter(Mandatory=$false)]
    [object]$DiscoveryRequired = $(if ($env:DISCOVERY_REQUIRED) { $env:DISCOVERY_REQUIRED } else { $true }),

    [Parameter(Mandatory=$false)]
    [string]$RegistryUrl = $(if ($env:REGISTRY_URL) { $env:REGISTRY_URL } elseif ($env:REDIS_URL) { $env:REDIS_URL } else { "redis://127.0.0.1:6379" }),

    [Parameter(Mandatory=$false)]
    [string]$RedisUrl = $(if ($env:REDIS_URL) { $env:REDIS_URL } elseif ($env:REGISTRY_URL) { $env:REGISTRY_URL } else { "redis://127.0.0.1:6379" }),

    [Parameter(Mandatory=$false)]
    [string]$RegistryKeyPrefix = $(if ($env:REGISTRY_KEY_PREFIX) { $env:REGISTRY_KEY_PREFIX } elseif ($env:REDIS_KEY_PREFIX) { $env:REDIS_KEY_PREFIX } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$RegistryFixturePath = $(if ($env:MYSERVER_REGISTRY_FIXTURE_PATH) { $env:MYSERVER_REGISTRY_FIXTURE_PATH } else { "" }),

    [Parameter(Mandatory=$false)]
    # Manual local startup hint only; player/control endpoints are resolved by registry discovery.
    [int]$OldGamePort = $(if ($env:MYSERVER_OLD_GAME_PORT) { [int]$env:MYSERVER_OLD_GAME_PORT } else { 7000 }),

    [Parameter(Mandatory=$false)]
    # Manual local startup hint only; player/control endpoints are resolved by registry discovery.
    [int]$NewGamePort = $(if ($env:MYSERVER_NEW_GAME_PORT) { [int]$env:MYSERVER_NEW_GAME_PORT } else { 7001 }),

    [Parameter(Mandatory=$false)]
    [Alias("OldAdminHost")]
    # Local/manual fallback only. Strict/test/production runs must use registry discovery.
    [string]$LocalFallbackOldAdminHost = $(if ($env:MYSERVER_OLD_GAME_ADMIN_HOST) { $env:MYSERVER_OLD_GAME_ADMIN_HOST } else { "127.0.0.1" }),

    [Parameter(Mandatory=$false)]
    [Alias("OldAdminPort")]
    # Local/manual fallback only. Strict/test/production runs must use registry discovery.
    [int]$LocalFallbackOldAdminPort = $(if ($env:MYSERVER_OLD_GAME_ADMIN_PORT) { [int]$env:MYSERVER_OLD_GAME_ADMIN_PORT } else { 7500 }),

    [Parameter(Mandatory=$false)]
    [string]$OldAdminToken = $(if ($env:MYSERVER_OLD_GAME_ADMIN_TOKEN) { $env:MYSERVER_OLD_GAME_ADMIN_TOKEN } elseif ($env:GAME_ADMIN_TOKEN) { $env:GAME_ADMIN_TOKEN } else { "dev-only-change-this-game-admin-token" }),

    [Parameter(Mandatory=$false)]
    [Alias("NewAdminHost")]
    # Local/manual fallback only. Strict/test/production runs must use registry discovery.
    [string]$LocalFallbackNewAdminHost = $(if ($env:MYSERVER_NEW_GAME_ADMIN_HOST) { $env:MYSERVER_NEW_GAME_ADMIN_HOST } else { "127.0.0.1" }),

    [Parameter(Mandatory=$false)]
    [Alias("NewAdminPort")]
    # Local/manual fallback only. Strict/test/production runs must use registry discovery.
    [int]$LocalFallbackNewAdminPort = $(if ($env:MYSERVER_NEW_GAME_ADMIN_PORT) { [int]$env:MYSERVER_NEW_GAME_ADMIN_PORT } else { 7501 }),

    [Parameter(Mandatory=$false)]
    [string]$NewAdminToken = $(if ($env:MYSERVER_NEW_GAME_ADMIN_TOKEN) { $env:MYSERVER_NEW_GAME_ADMIN_TOKEN } elseif ($env:GAME_ADMIN_TOKEN) { $env:GAME_ADMIN_TOKEN } else { "dev-only-change-this-game-admin-token" }),

    [Parameter(Mandatory=$false)]
    [Alias("AuthBaseUrl")]
    # Local/manual fallback only. Strict/test/production runs must use registry discovery.
    [string]$LocalFallbackAuthBaseUrl = $(if ($env:MYSERVER_AUTH_BASE_URL) { $env:MYSERVER_AUTH_BASE_URL } else { "http://127.0.0.1:3000" }),

    [Parameter(Mandatory=$false)]
    [string]$ServiceToken = $(if ($env:MYSERVER_INTERNAL_API_TOKEN) { $env:MYSERVER_INTERNAL_API_TOKEN } elseif ($env:INTERNAL_API_TOKEN) { $env:INTERNAL_API_TOKEN } else { "" }),

    [Parameter(Mandatory=$false)]
    [Alias("ProxyAdminUrl")]
    # Local/manual fallback only. Strict/test/production runs must use registry discovery.
    [string]$LocalFallbackProxyAdminUrl = $(if ($env:MYSERVER_PROXY_ADMIN_URL) { $env:MYSERVER_PROXY_ADMIN_URL } else { "http://127.0.0.1:7101" }),

    [Parameter(Mandatory=$false)]
    [string]$ProxyAdminToken = $(if ($env:PROXY_ADMIN_TOKEN) { $env:PROXY_ADMIN_TOKEN } else { "dev-only-change-this-proxy-admin-token" }),

    [Parameter(Mandatory=$false)]
    [int]$TimeoutMs = $(if ($env:MYSERVER_ROLLOUT_TIMEOUT_MS) { [int]$env:MYSERVER_ROLLOUT_TIMEOUT_MS } else { 5000 }),

    [Parameter(Mandatory=$false)]
    [int]$OldProcessPid = $(if ($env:MYSERVER_OLD_PROCESS_PID) { [int]$env:MYSERVER_OLD_PROCESS_PID } else { 0 }),

    [Parameter(Mandatory=$false)]
    [string]$OldProcessPidFile = $(if ($env:MYSERVER_OLD_PROCESS_PID_FILE) { $env:MYSERVER_OLD_PROCESS_PID_FILE } else { "" }),

    [Parameter(Mandatory=$false)]
    [string]$OldProcessPidName = $(if ($env:MYSERVER_OLD_PROCESS_PID_NAME) { $env:MYSERVER_OLD_PROCESS_PID_NAME } else { "game-server-old" }),

    [Parameter(Mandatory=$false)]
    [int]$ShutdownWaitTimeoutMs = $(if ($env:MYSERVER_SHUTDOWN_WAIT_TIMEOUT_MS) { [int]$env:MYSERVER_SHUTDOWN_WAIT_TIMEOUT_MS } else { 30000 }),

    [Parameter(Mandatory=$false)]
    [string]$AdminActor = $(if ($env:MYSERVER_ADMIN_ACTOR) { $env:MYSERVER_ADMIN_ACTOR } elseif ($env:MYSERVER_PROXY_ADMIN_ACTOR) { $env:MYSERVER_PROXY_ADMIN_ACTOR } else { "rollout-three-process-drill" }),

    [Parameter(Mandatory=$false)]
    [string]$ReportPath = $(if ($env:MYSERVER_ROLLOUT_REPORT_PATH) { $env:MYSERVER_ROLLOUT_REPORT_PATH } else { "" })
)

$ErrorActionPreference = "Stop"

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$TransferCli = Join-Path $ProjectRoot "tools\mock-client\src\rollout-transfer-cli.js"
$MockClientIndex = Join-Path $ProjectRoot "tools\mock-client\src\index.js"
if ([string]::IsNullOrWhiteSpace($ReportPath)) {
    $ReportPath = Join-Path $ProjectRoot ".tmp\rollout-three-process-drill-report.json"
}
$script:StageResults = @()
$script:TransferCliResult = $null
$script:ShutdownResult = $null
$script:OldProcessManager = $null
$script:Discovery = $null
$script:RegistryEnabledValue = $false
$script:DiscoveryRequiredValue = $false
$script:ReportWritten = $false
$script:StartedAt = (Get-Date).ToUniversalTime().ToString("o")

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

    if ($Value -match "^[A-Za-z0-9_.,:/=@%+\\-]+$") {
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

function Test-RolloutIdentifier {
    param([Parameter(Mandatory=$true)][AllowEmptyString()][string]$Value)
    return $Value -match "^[A-Za-z0-9_.:@-]{1,128}$"
}

function Test-AdminActor {
    param([Parameter(Mandatory=$true)][AllowEmptyString()][string]$Value)
    return $Value -match "^[A-Za-z0-9_.@-]{1,128}$"
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

function Test-RegistryEnabled {
    return [bool]$script:RegistryEnabledValue
}

function Test-DiscoveryRequired {
    return [bool]$script:DiscoveryRequiredValue
}

function Test-StrictDiscoveryEnvironment {
    $names = @($EnvironmentName, $env:APP_ENV, $env:NODE_ENV)
    foreach ($name in $names) {
        if ($null -ne $name -and ([string]$name).Trim() -match "^(prod|production|staging|test|testing)$") {
            return $true
        }
    }
    return $false
}

function ConvertTo-BooleanOption {
    param(
        [Parameter(Mandatory=$true)]$Value,
        [Parameter(Mandatory=$true)][string]$Name
    )

    if ($Value -is [bool]) {
        return [bool]$Value
    }
    if ($Value -is [int]) {
        return [int]$Value -ne 0
    }

    $text = [string]$Value
    if ($text -match "^(1|true|yes|on)$") {
        return $true
    }
    if ($text -match "^(0|false|no|off)$") {
        return $false
    }
    throw "$Name must be a boolean value: true/false, 1/0, yes/no, on/off"
}

function Resolve-RegistryFixturePath {
    if ([string]::IsNullOrWhiteSpace($RegistryFixturePath)) {
        return ""
    }
    if ([System.IO.Path]::IsPathRooted($RegistryFixturePath)) {
        return $RegistryFixturePath
    }
    return (Join-Path $ProjectRoot $RegistryFixturePath)
}

function Invoke-RegistryDiscoveryNode {
    $fixturePath = Resolve-RegistryFixturePath
    $schemaPath = (Join-Path $ProjectRoot "packages\service-registry\node\registry-schema.js")
    $schemaUrl = ([Uri]$schemaPath).AbsoluteUri
    $nodeScript = @'
import fs from "node:fs";
import process from "node:process";
const { normalizeServiceInstance } = await import(process.env.MYSERVER_REGISTRY_SCHEMA_URL);

const boolValue = (value) => /^(1|true|yes|on)$/i.test(String(value || ""));
const prefix = process.env.MYSERVER_REGISTRY_KEY_PREFIX || "";
const fixturePath = process.env.MYSERVER_REGISTRY_FIXTURE_PATH || "";
const redisUrl = process.env.MYSERVER_REGISTRY_URL || process.env.REGISTRY_URL || process.env.REDIS_URL || "redis://127.0.0.1:6379";
const registryEnabled = boolValue(process.env.MYSERVER_REGISTRY_ENABLED);

function emit(payload) {
  console.log(JSON.stringify(payload, null, 2));
}

function serviceKeys(serviceName) {
  return {
    instances: `${prefix}service:${serviceName}:instances:*`,
    key: (instanceId) => `${prefix}service:${serviceName}:instances:${instanceId}`,
    heartbeat: (instanceId) => `${prefix}heartbeat:${serviceName}:${instanceId}`
  };
}

function normalizeList(values) {
  return values.map((value) => normalizeServiceInstance(value)).filter(Boolean);
}

function fixtureInstances(payload, serviceName) {
  if (Array.isArray(payload)) {
    return normalizeList(payload.filter((item) => item?.name === serviceName));
  }
  if (Array.isArray(payload?.instances?.[serviceName])) {
    return normalizeList(payload.instances[serviceName]);
  }
  if (Array.isArray(payload?.services?.[serviceName])) {
    return normalizeList(payload.services[serviceName]);
  }
  return [];
}

async function readFixture() {
  const payload = JSON.parse(fs.readFileSync(fixturePath, "utf8"));
  return {
    source: "fixture",
    services: {
      "game-server": fixtureInstances(payload, "game-server"),
      "game-proxy": fixtureInstances(payload, "game-proxy"),
      "auth-http": fixtureInstances(payload, "auth-http")
    }
  };
}

async function readRedis() {
  const { default: Redis } = await import("ioredis");
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableOfflineQueue: false
  });
  try {
    await redis.connect();
    const services = {};
    for (const serviceName of ["game-server", "game-proxy", "auth-http"]) {
      const keys = serviceKeys(serviceName);
      const instanceKeys = [];
      let cursor = "0";
      do {
        const [nextCursor, batch] = await redis.scan(cursor, "MATCH", keys.instances, "COUNT", 100);
        cursor = nextCursor;
        instanceKeys.push(...batch);
      } while (cursor !== "0");

      const instances = [];
      for (const key of instanceKeys.sort()) {
        const instanceId = key.split(":").at(-1);
        if (!instanceId) continue;
        const heartbeatExists = await redis.exists(keys.heartbeat(instanceId));
        if (!heartbeatExists) continue;
        const data = await redis.hget(key, "data");
        if (!data) continue;
        const normalized = normalizeServiceInstance(JSON.parse(data));
        if (normalized) instances.push(normalized);
      }
      services[serviceName] = instances;
    }
    return { source: "redis", services };
  } finally {
    redis.disconnect();
  }
}

function expectedVisibility(endpointName) {
  if (endpointName === "admin") return "admin";
  if (endpointName === "internal") return "internal";
  return "";
}

function endpointMatches(endpoint, endpointName, protocols, visibility = "") {
  return endpoint?.name === endpointName &&
    (!visibility || endpoint.visibility === visibility) &&
    protocols.includes(endpoint.protocol) &&
    endpoint.healthy !== false &&
    endpoint.host &&
    endpoint.port > 0;
}

function selectInstance(instances, instanceId, serviceName) {
  const candidates = instances.filter((instance) => instance.healthy !== false && instance.weight > 0);
  const match = candidates.find((instance) => instance.id === instanceId);
  if (!match) {
    throw new Error(`${serviceName} instance not found or unhealthy: ${instanceId}`);
  }
  return match;
}

function selectEndpoint(instance, endpointName, protocols, label) {
  const visibility = expectedVisibility(endpointName);
  const endpoint = instance.endpoints.find((item) => endpointMatches(item, endpointName, protocols, visibility));
  if (!endpoint) {
    throw new Error(`${label} endpoint not found: instance=${instance.id}, endpoint=${endpointName}, protocols=${protocols.join("|")}${visibility ? `, visibility=${visibility}` : ""}`);
  }
  return {
    instanceId: instance.id,
    endpointName: endpoint.name,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    url: endpoint.protocol === "http" || endpoint.protocol === "https" ? `${endpoint.protocol}://${endpoint.host}:${endpoint.port}` : "",
    source: "registry",
    metadata: endpoint.metadata || {}
  };
}

function selectSingletonEndpoint(instances, requestedInstanceId, endpointName, protocols, serviceName) {
  const visibility = expectedVisibility(endpointName);
  const candidates = instances
    .filter((instance) => instance.healthy !== false && instance.weight > 0)
    .flatMap((instance) => instance.endpoints
      .filter((endpoint) => endpointMatches(endpoint, endpointName, protocols, visibility))
      .map((endpoint) => ({ instance, endpoint })));

  const filtered = requestedInstanceId
    ? candidates.filter(({ instance }) => instance.id === requestedInstanceId)
    : candidates;

  if (filtered.length === 0) {
    throw new Error(`${serviceName}.${endpointName} endpoint not found${requestedInstanceId ? ` for instance ${requestedInstanceId}` : ""}`);
  }
  if (!requestedInstanceId && filtered.length > 1) {
    throw new Error(`${serviceName}.${endpointName} has multiple candidates; pass the instance id`);
  }

  const { instance, endpoint } = filtered[0];
  return {
    instanceId: instance.id,
    endpointName: endpoint.name,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    url: endpoint.protocol === "http" || endpoint.protocol === "https" ? `${endpoint.protocol}://${endpoint.host}:${endpoint.port}` : "",
    source: "registry",
    metadata: endpoint.metadata || {}
  };
}

try {
  if (!fixturePath && !registryEnabled) {
    throw new Error("REGISTRY_ENABLED=false");
  }
  const snapshot = fixturePath ? await readFixture() : await readRedis();
  const oldInstance = selectInstance(snapshot.services["game-server"], process.env.MYSERVER_OLD_SERVER_ID, "game-server");
  const newInstance = selectInstance(snapshot.services["game-server"], process.env.MYSERVER_NEW_SERVER_ID, "game-server");
  const oldAdmin = selectEndpoint(oldInstance, "admin", ["tcp"], "old game-server admin");
  const newAdmin = selectEndpoint(newInstance, "admin", ["tcp"], "new game-server admin");
  const proxyAdmin = selectSingletonEndpoint(
    snapshot.services["game-proxy"],
    process.env.MYSERVER_PROXY_INSTANCE_ID || "",
    "admin",
    ["http", "https"],
    "game-proxy"
  );

  let authHttp = null;
  try {
    authHttp = selectSingletonEndpoint(
      snapshot.services["auth-http"],
      process.env.MYSERVER_AUTH_INSTANCE_ID || "",
      "internal",
      ["http", "https"],
      "auth-http"
    );
  } catch {
    authHttp = null;
  }

  emit({
    ok: true,
    source: snapshot.source,
    redisUrl,
    registryKeyPrefix: prefix,
    oldGameServerAdmin: oldAdmin,
    newGameServerAdmin: newAdmin,
    gameProxyAdmin: proxyAdmin,
    authHttp,
    serviceCounts: Object.fromEntries(Object.entries(snapshot.services).map(([key, value]) => [key, value.length]))
  });
} catch (error) {
  emit({
    ok: false,
    source: fixturePath ? "fixture" : "redis",
    redisUrl,
    registryKeyPrefix: prefix,
    error: error.stack || error.message || String(error)
  });
  process.exitCode = 1;
}
'@

    $env:MYSERVER_REGISTRY_ENABLED = if (Test-RegistryEnabled) { "true" } else { "false" }
    $env:MYSERVER_REGISTRY_URL = if (-not [string]::IsNullOrWhiteSpace($RegistryUrl)) { $RegistryUrl } else { $RedisUrl }
    $env:MYSERVER_REGISTRY_KEY_PREFIX = $RegistryKeyPrefix
    $env:MYSERVER_REGISTRY_FIXTURE_PATH = $fixturePath
    $env:MYSERVER_REGISTRY_SCHEMA_URL = $schemaUrl
    $env:MYSERVER_OLD_SERVER_ID = $OldServerId
    $env:MYSERVER_NEW_SERVER_ID = $NewServerId
    $env:MYSERVER_PROXY_INSTANCE_ID = $ProxyInstanceId
    $env:MYSERVER_AUTH_INSTANCE_ID = $AuthInstanceId

    $tempScriptDir = Join-Path $ProjectRoot ".tmp"
    if (-not (Test-Path $tempScriptDir)) {
        New-Item -ItemType Directory -Path $tempScriptDir -Force | Out-Null
    }
    $tempScriptPath = Join-Path $tempScriptDir ("rollout-discovery-{0}.mjs" -f ([guid]::NewGuid().ToString("N")))
    Set-Content -LiteralPath $tempScriptPath -Value $nodeScript -Encoding UTF8
    try {
        $output = & node $tempScriptPath 2>&1
        $exitCode = $LASTEXITCODE
    } finally {
        Remove-Item -LiteralPath $tempScriptPath -Force -ErrorAction SilentlyContinue
    }
    $text = ($output | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
    if ([string]::IsNullOrWhiteSpace($text)) {
        throw "registry discovery produced no output"
    }

    try {
        $json = $text | ConvertFrom-Json
    } catch {
        throw "registry discovery produced invalid JSON: $text"
    }

    if ($exitCode -ne 0 -or -not $json.ok) {
        $message = if ($json.error) { $json.error } else { "registry discovery failed with exit code $exitCode" }
        throw $message
    }

    return $json
}

function New-LocalFallbackDiscovery {
    return [pscustomobject]@{
        mode = "local-fallback"
        source = "parameters"
        registryEnabled = [bool]$script:RegistryEnabledValue
        discoveryRequired = [bool]$script:DiscoveryRequiredValue
        environmentName = $EnvironmentName
        registryUrl = if (-not [string]::IsNullOrWhiteSpace($RegistryUrl)) { $RegistryUrl } else { $RedisUrl }
        registryKeyPrefix = $RegistryKeyPrefix
        warnings = @("local fallback is only valid when registry is disabled or discovery is non-required")
        oldGameServerAdmin = [pscustomobject]@{
            instanceId = $OldServerId
            endpointName = "admin"
            protocol = "tcp"
            host = $LocalFallbackOldAdminHost
            port = $LocalFallbackOldAdminPort
            url = ""
            source = "local-fallback"
        }
        newGameServerAdmin = [pscustomobject]@{
            instanceId = $NewServerId
            endpointName = "admin"
            protocol = "tcp"
            host = $LocalFallbackNewAdminHost
            port = $LocalFallbackNewAdminPort
            url = ""
            source = "local-fallback"
        }
        gameProxyAdmin = [pscustomobject]@{
            instanceId = if ($ProxyInstanceId) { $ProxyInstanceId } else { "<local-fallback>" }
            endpointName = "admin"
            protocol = "http"
            host = (Get-UriEndpoint $LocalFallbackProxyAdminUrl).host
            port = (Get-UriEndpoint $LocalFallbackProxyAdminUrl).port
            url = $LocalFallbackProxyAdminUrl
            source = "local-fallback"
        }
        authHttp = [pscustomobject]@{
            instanceId = if ($AuthInstanceId) { $AuthInstanceId } else { "<local-fallback>" }
            endpointName = "internal"
            protocol = if ($LocalFallbackAuthBaseUrl.StartsWith("https://")) { "https" } else { "http" }
            host = (Get-UriEndpoint $LocalFallbackAuthBaseUrl).host
            port = (Get-UriEndpoint $LocalFallbackAuthBaseUrl).port
            url = $LocalFallbackAuthBaseUrl
            source = "local-fallback"
        }
        serviceCounts = [pscustomobject]@{}
    }
}

function Resolve-RolloutDiscovery {
    $warnings = @()
    if (Test-RegistryEnabled) {
        try {
            $registryResult = Invoke-RegistryDiscoveryNode
            $auth = $registryResult.authHttp
            if ($null -eq $auth) {
                if (Test-DiscoveryRequired) {
                    throw "auth-http.internal endpoint not found in registry"
                } else {
                    $authEndpoint = Get-UriEndpoint $LocalFallbackAuthBaseUrl
                    $auth = [pscustomobject]@{
                        instanceId = if ($AuthInstanceId) { $AuthInstanceId } else { "<fallback>" }
                        endpointName = "internal"
                        protocol = if ($LocalFallbackAuthBaseUrl.StartsWith("https://")) { "https" } else { "http" }
                        host = $authEndpoint.host
                        port = $authEndpoint.port
                        url = $LocalFallbackAuthBaseUrl
                        source = "fallback-auth-base-url"
                    }
                    $warnings += "auth-http.internal endpoint not found in registry; using AuthBaseUrl fallback because DiscoveryRequired=false"
                }
            }

            return [pscustomobject]@{
                mode = "registry"
                source = $registryResult.source
                registryEnabled = [bool]$script:RegistryEnabledValue
                discoveryRequired = [bool]$script:DiscoveryRequiredValue
                environmentName = $EnvironmentName
                registryUrl = $registryResult.redisUrl
                registryKeyPrefix = $registryResult.registryKeyPrefix
                warnings = $warnings
                oldGameServerAdmin = $registryResult.oldGameServerAdmin
                newGameServerAdmin = $registryResult.newGameServerAdmin
                gameProxyAdmin = $registryResult.gameProxyAdmin
                authHttp = $auth
                serviceCounts = $registryResult.serviceCounts
            }
        } catch {
            if (Test-DiscoveryRequired) {
                throw "Required registry discovery failed: $($_.Exception.Message)"
            }
            $warnings += "registry discovery failed and DiscoveryRequired=false: $($_.Exception.Message)"
        }
    } elseif (Test-DiscoveryRequired) {
        throw "Required registry discovery failed: REGISTRY_ENABLED=false"
    } else {
        $warnings += "REGISTRY_ENABLED=false; using local fallback endpoints"
    }

    $fallback = New-LocalFallbackDiscovery
    $fallback.warnings = @($fallback.warnings) + $warnings
    return $fallback
}

function Write-DiscoverySummary {
    param([Parameter(Mandatory=$true)]$Discovery)

    Write-Section "Discovery"
    Write-Host "Mode: $($Discovery.mode)" -ForegroundColor Gray
    Write-Host "Source: $($Discovery.source)" -ForegroundColor Gray
    Write-Host "EnvironmentName: $($Discovery.environmentName)" -ForegroundColor Gray
    Write-Host "RegistryEnabled: $($Discovery.registryEnabled)" -ForegroundColor Gray
    Write-Host "DiscoveryRequired: $($Discovery.discoveryRequired)" -ForegroundColor Gray
    Write-Host "RegistryUrl: $($Discovery.registryUrl)" -ForegroundColor Gray
    Write-Host "RegistryKeyPrefix: $($Discovery.registryKeyPrefix)" -ForegroundColor Gray
    Write-Host "OldGameServerAdmin: $($Discovery.oldGameServerAdmin.instanceId) $($Discovery.oldGameServerAdmin.host):$($Discovery.oldGameServerAdmin.port)" -ForegroundColor Gray
    Write-Host "NewGameServerAdmin: $($Discovery.newGameServerAdmin.instanceId) $($Discovery.newGameServerAdmin.host):$($Discovery.newGameServerAdmin.port)" -ForegroundColor Gray
    Write-Host "GameProxyAdmin: $($Discovery.gameProxyAdmin.instanceId) $($Discovery.gameProxyAdmin.url)" -ForegroundColor Gray
    Write-Host "AuthHttp: $($Discovery.authHttp.instanceId) $($Discovery.authHttp.url)" -ForegroundColor Gray
    foreach ($warning in @($Discovery.warnings)) {
        if (-not [string]::IsNullOrWhiteSpace($warning)) {
            Write-Warning $warning
        }
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

function Invoke-ExternalJsonStep {
    param(
        [Parameter(Mandatory=$true)][string]$Name,
        [Parameter(Mandatory=$true)][string]$FilePath,
        [Parameter(Mandatory=$true)][string[]]$Arguments
    )

    Write-Host "Running $Name" -ForegroundColor Yellow
    $output = & $FilePath @Arguments 2>&1
    $exitCode = $LASTEXITCODE
    $text = ($output | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
    if (-not [string]::IsNullOrWhiteSpace($text)) {
        Write-Host $text
    }

    try {
        $json = $text | ConvertFrom-Json
    } catch {
        throw "$Name did not produce valid JSON output. exitCode=$exitCode output=$text"
    }

    return [pscustomobject]@{
        exitCode = $exitCode
        json = $json
        text = $text
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

function Resolve-OldProcessManager {
    $resolvedPid = [int]$OldProcessPid
    $source = if ($resolvedPid -gt 0) { "parameter" } else { "" }
    $resolvedFile = ""

    if ($resolvedPid -le 0 -and -not [string]::IsNullOrWhiteSpace($OldProcessPidFile)) {
        $candidate = if ([System.IO.Path]::IsPathRooted($OldProcessPidFile)) {
            $OldProcessPidFile
        } else {
            Join-Path $ProjectRoot $OldProcessPidFile
        }
        $resolvedFile = $candidate
        if (Test-Path $candidate) {
            try {
                $json = Get-Content -LiteralPath $candidate -Raw | ConvertFrom-Json
                $processes = @()
                if ($json.processes) {
                    $processes = @($json.processes)
                } elseif ($json -is [array]) {
                    $processes = @($json)
                } elseif ($json.pid) {
                    $processes = @($json)
                }

                $match = $processes | Where-Object { $_.name -eq $OldProcessPidName } | Select-Object -First 1
                if (-not $match -and $processes.Count -eq 1) {
                    $match = $processes[0]
                }
                if ($match -and $match.pid) {
                    $resolvedPid = [int]$match.pid
                    $source = "pid-file"
                }
            } catch {
                $script:StageResults += [pscustomobject]@{
                    stage = "old-process-pid-resolve"
                    status = "warning"
                    detail = "failed to read ${candidate}: $($_.Exception.Message)"
                }
            }
        } else {
            $script:StageResults += [pscustomobject]@{
                stage = "old-process-pid-resolve"
                status = "warning"
                detail = "pid file not found: $candidate"
            }
        }
    }

    $exists = $false
    if ($resolvedPid -gt 0) {
        $exists = [bool](Get-Process -Id $resolvedPid -ErrorAction SilentlyContinue)
    }

    return [pscustomobject]@{
        enabled = $resolvedPid -gt 0
        pid = $resolvedPid
        pidName = $OldProcessPidName
        pidFile = $resolvedFile
        source = $source
        waitTimeoutMs = $ShutdownWaitTimeoutMs
        processExistsAtPreflight = $exists
    }
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

function New-EndpointResolutionReport {
    param(
        [Parameter(Mandatory=$false)]$Endpoint,
        [Parameter(Mandatory=$false)][AllowEmptyString()][string]$RegistrySource = ""
    )

    if ($null -eq $Endpoint) {
        return $null
    }

    $source = if ($Endpoint.source) { $Endpoint.source } else { $script:Discovery.mode }
    $url = if ($Endpoint.url) { $Endpoint.url } else { "" }
    return [pscustomobject]@{
        instanceId = $Endpoint.instanceId
        endpointName = $Endpoint.endpointName
        protocol = $Endpoint.protocol
        host = $Endpoint.host
        port = if ($null -ne $Endpoint.port) { [int]$Endpoint.port } else { $null }
        url = $url
        source = $source
        registrySource = if ($source -eq "registry") { $RegistrySource } else { $null }
    }
}

function New-ResolvedEndpointsReport {
    if ($null -eq $script:Discovery) {
        return $null
    }

    return [pscustomobject]@{
        oldGameServerAdmin = "$($script:Discovery.oldGameServerAdmin.host):$($script:Discovery.oldGameServerAdmin.port)"
        newGameServerAdmin = "$($script:Discovery.newGameServerAdmin.host):$($script:Discovery.newGameServerAdmin.port)"
        authHttp = $script:Discovery.authHttp.url
        gameProxyAdmin = $script:Discovery.gameProxyAdmin.url
        source = $script:Discovery.mode
        discoverySource = $script:Discovery.source
        endpointSources = [pscustomobject]@{
            oldGameServerAdmin = $script:Discovery.oldGameServerAdmin.source
            newGameServerAdmin = $script:Discovery.newGameServerAdmin.source
            authHttp = $script:Discovery.authHttp.source
            gameProxyAdmin = $script:Discovery.gameProxyAdmin.source
        }
        endpoints = [pscustomobject]@{
            oldGameServerAdmin = New-EndpointResolutionReport -Endpoint $script:Discovery.oldGameServerAdmin -RegistrySource $script:Discovery.source
            newGameServerAdmin = New-EndpointResolutionReport -Endpoint $script:Discovery.newGameServerAdmin -RegistrySource $script:Discovery.source
            authHttp = New-EndpointResolutionReport -Endpoint $script:Discovery.authHttp -RegistrySource $script:Discovery.source
            gameProxyAdmin = New-EndpointResolutionReport -Endpoint $script:Discovery.gameProxyAdmin -RegistrySource $script:Discovery.source
        }
    }
}

function New-RunReport {
    $mode = if ($ExecuteSteps) { "execute" } else { "dry-run" }
    $roomValue = if ($RoomId) { $RoomId } else { $null }
    $rolloutValue = if ($RolloutEpoch) { $RolloutEpoch } else { $null }

    return [pscustomobject]@{
        ok = -not ($script:StageResults | Where-Object { $_.status -eq "failed" })
        mode = $mode
        startedAt = $script:StartedAt
        completedAt = (Get-Date).ToUniversalTime().ToString("o")
        projectRoot = $ProjectRoot
        script = "scripts/ops/rollout-three-process-drill.ps1"
        inputs = [pscustomobject]@{
            roomId = $roomValue
            roomIdPlaceholder = if ($RoomId) { $RoomId } else { "<ROOM_ID>" }
            rolloutEpoch = $rolloutValue
            rolloutEpochPlaceholder = if ($RolloutEpoch) { $RolloutEpoch } else { "<ROLLOUT_EPOCH>" }
            oldServerId = $OldServerId
            newServerId = $NewServerId
            proxyInstanceId = if ($ProxyInstanceId) { $ProxyInstanceId } else { $null }
            authInstanceId = if ($AuthInstanceId) { $AuthInstanceId } else { $null }
            environmentName = $EnvironmentName
            registryEnabled = [bool]$script:RegistryEnabledValue
            discoveryRequired = [bool]$script:DiscoveryRequiredValue
            registryUrl = if (-not [string]::IsNullOrWhiteSpace($RegistryUrl)) { $RegistryUrl } else { $RedisUrl }
            registryKeyPrefix = $RegistryKeyPrefix
            registryFixturePath = if ($RegistryFixturePath) { Resolve-RegistryFixturePath } else { $null }
            oldGamePort = $OldGamePort
            newGamePort = $NewGamePort
            localFallbackOldAdminEndpoint = "$($LocalFallbackOldAdminHost):$($LocalFallbackOldAdminPort)"
            localFallbackNewAdminEndpoint = "$($LocalFallbackNewAdminHost):$($LocalFallbackNewAdminPort)"
            localFallbackAuthBaseUrl = $LocalFallbackAuthBaseUrl
            localFallbackProxyAdminUrl = $LocalFallbackProxyAdminUrl
            proxyAdminActor = $AdminActor
            timeoutMs = $TimeoutMs
            shutdownWaitTimeoutMs = $ShutdownWaitTimeoutMs
            oldProcessManager = $script:OldProcessManager
            tokenStates = [pscustomobject]@{
                oldAdmin = Mask-TokenState $OldAdminToken
                newAdmin = Mask-TokenState $NewAdminToken
                proxyAdmin = Mask-TokenState $ProxyAdminToken
                authInternalService = Mask-TokenState $ServiceToken
            }
        }
        resolvedEndpoints = New-ResolvedEndpointsReport
        discovery = $script:Discovery
        safety = [pscustomobject]@{
            startsServices = $false
            executeSteps = [bool]$ExecuteSteps
            skipPortProbe = [bool]$SkipPortProbe
            allowShutdownRequest = [bool]$AllowShutdownRequest
            skipShutdownRequest = [bool]$SkipShutdownRequest
            shutdownRequestCanRun = [bool]($ExecuteSteps -and $AllowShutdownRequest -and -not $SkipShutdownRequest)
            waitsForOldProcessExit = [bool]($ExecuteSteps -and $AllowShutdownRequest -and -not $SkipShutdownRequest -and $script:OldProcessManager -and $script:OldProcessManager.enabled)
        }
        stages = $script:StageResults
        transfer = $script:TransferCliResult
        shutdown = $script:ShutdownResult
    }
}

function Write-RunReport {
    if ([string]::IsNullOrWhiteSpace($ReportPath)) {
        return
    }

    try {
        $parent = Split-Path -Parent $ReportPath
        if (-not [string]::IsNullOrWhiteSpace($parent) -and -not (Test-Path $parent)) {
            New-Item -ItemType Directory -Path $parent -Force | Out-Null
        }

        New-RunReport | ConvertTo-Json -Depth 100 | Set-Content -Path $ReportPath -Encoding UTF8
        $script:ReportWritten = $true
        Write-Host "Report: $ReportPath" -ForegroundColor Gray
    } catch {
        Write-Warning "failed to write report ${ReportPath}: $($_.Exception.Message)"
    }
}

trap {
    if (-not $script:ReportWritten) {
        if (-not ($script:StageResults | Where-Object { $_.status -eq "failed" })) {
            Add-StageResult "script" "failed" $_.Exception.Message
        }
        Write-RunSummary
        Write-RunReport
    }
    throw
}

$displayRoomId = if ($RoomId) { $RoomId } else { "<ROOM_ID>" }
$displayRolloutEpoch = if ($RolloutEpoch) { $RolloutEpoch } else { "<ROLLOUT_EPOCH>" }
$script:OldProcessManager = Resolve-OldProcessManager
$script:RegistryEnabledValue = ConvertTo-BooleanOption -Value $RegistryEnabled -Name "RegistryEnabled"
$script:DiscoveryRequiredValue = ConvertTo-BooleanOption -Value $DiscoveryRequired -Name "DiscoveryRequired"
if (Test-StrictDiscoveryEnvironment) {
    $script:DiscoveryRequiredValue = $true
}
$script:Discovery = Resolve-RolloutDiscovery

$OldAdminHost = $script:Discovery.oldGameServerAdmin.host
$OldAdminPort = [int]$script:Discovery.oldGameServerAdmin.port
$NewAdminHost = $script:Discovery.newGameServerAdmin.host
$NewAdminPort = [int]$script:Discovery.newGameServerAdmin.port
$ProxyAdminUrl = $script:Discovery.gameProxyAdmin.url
$AuthBaseUrl = $script:Discovery.authHttp.url

Write-Section "Mode"
if ($ExecuteSteps) {
    Write-Host "EXECUTE mode: control endpoints will be called. Services must already be running." -ForegroundColor Yellow
} else {
    Write-Host "DRY-RUN mode: no service writes, no service starts, no integration stack execution." -ForegroundColor Green
}

Write-Host "ProjectRoot: $ProjectRoot" -ForegroundColor Gray
Write-Host "ReportPath: $ReportPath" -ForegroundColor Gray
Write-Host "RoomId: $displayRoomId" -ForegroundColor Gray
Write-Host "RolloutEpoch: $displayRolloutEpoch" -ForegroundColor Gray
Write-Host "OldServerId: $OldServerId" -ForegroundColor Gray
Write-Host "NewServerId: $NewServerId" -ForegroundColor Gray
Write-Host "ProxyInstanceId: $(if ($ProxyInstanceId) { $ProxyInstanceId } else { '<auto>' })" -ForegroundColor Gray
Write-Host "AuthInstanceId: $(if ($AuthInstanceId) { $AuthInstanceId } else { '<auto>' })" -ForegroundColor Gray
if ($script:OldProcessManager.enabled) {
    Write-Host "OldProcessPid: $($script:OldProcessManager.pid) ($($script:OldProcessManager.source))" -ForegroundColor Gray
    Write-Host "OldProcessWaitTimeoutMs: $($script:OldProcessManager.waitTimeoutMs)" -ForegroundColor Gray
} else {
    Write-Host "OldProcessPid: not set; shutdown request will not verify local process exit" -ForegroundColor Yellow
}

Write-DiscoverySummary $script:Discovery

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
} elseif ($ExecuteSteps -and $RoomId -match "^<[^>]+>$") {
    $preflightErrors += "RoomId placeholder values are not allowed in -ExecuteSteps mode"
} elseif ($ExecuteSteps -and -not (Test-RolloutIdentifier $RoomId)) {
    $preflightErrors += "RoomId must be 1-128 chars matching [A-Za-z0-9_.:@-] in -ExecuteSteps mode"
}

if ($ExecuteSteps -and [string]::IsNullOrWhiteSpace($RolloutEpoch)) {
    $preflightErrors += "RolloutEpoch is required in -ExecuteSteps mode"
} elseif ($ExecuteSteps -and $RolloutEpoch -match "^<[^>]+>$") {
    $preflightErrors += "RolloutEpoch placeholder values are not allowed in -ExecuteSteps mode"
} elseif ($ExecuteSteps -and -not (Test-RolloutIdentifier $RolloutEpoch)) {
    $preflightErrors += "RolloutEpoch must be 1-128 chars matching [A-Za-z0-9_.:@-] in -ExecuteSteps mode"
}

if ($ExecuteSteps -and -not (Test-AdminActor $AdminActor)) {
    $preflightErrors += "AdminActor must be 1-128 chars matching [A-Za-z0-9_.@-] in -ExecuteSteps mode"
}

if ($ExecuteSteps -and $AllowShutdownRequest -and $script:OldProcessManager.enabled -and -not $script:OldProcessManager.processExistsAtPreflight) {
    $preflightErrors += "OldProcessPid $($script:OldProcessManager.pid) is not running before shutdown verification"
}

if ($ExecuteSteps -and $AllowShutdownRequest -and $script:OldProcessManager.enabled -and $ShutdownWaitTimeoutMs -le 0) {
    $preflightErrors += "ShutdownWaitTimeoutMs must be positive when old process exit verification is enabled"
}

if ($ExecuteSteps -and $AllowShutdownRequest -and -not [string]::IsNullOrWhiteSpace($OldProcessPidFile) -and -not $script:OldProcessManager.enabled) {
    $preflightErrors += "OldProcessPidFile was provided but no old process pid was resolved"
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

if ($script:Discovery.mode -eq "local-fallback") {
    if ($script:RegistryEnabledValue -and $script:DiscoveryRequiredValue) {
        $preflightErrors += "local fallback endpoints are forbidden when RegistryEnabled=true and DiscoveryRequired=true"
    } elseif (Test-StrictDiscoveryEnvironment) {
        $preflightErrors += "local fallback endpoints are forbidden when EnvironmentName, APP_ENV or NODE_ENV is production/staging/test"
    } else {
        $preflightWarnings += "using local fallback endpoints; this is only valid for registry disabled or discovery non-required local drills"
    }
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
    throw "Preflight failed"
}

Write-Section "Stage 0 - Manual Service Preparation"
Write-Host "This script never starts services. If needed, start dependencies and these processes in separate terminals." -ForegroundColor Yellow
Write-Host "The commands below are local/manual startup hints only; execute/test/production control endpoints come from the Discovery section." -ForegroundColor Yellow
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev/services/dev-auth.ps1")
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev/services/dev-game.ps1", "-InstanceId", $OldServerId, "-Port", [string]$OldGamePort, "-AdminPort", [string]$OldAdminPort)
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev/services/dev-game.ps1", "-InstanceId", $NewServerId, "-Port", [string]$NewGamePort, "-AdminPort", [string]$NewAdminPort)
Write-CommandLine @("powershell", "-ExecutionPolicy", "Bypass", "-File", "scripts/dev/services/dev-proxy.ps1")
Write-Host "Prerequisite: auth-http internal game-server admin client must resolve the old game-server admin endpoint through registry discovery or the explicit old instance id." -ForegroundColor Yellow
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
Write-Host "POST $configUri body { key=drain_mode_source, value=scripts/ops/rollout-three-process-drill.ps1 }" -ForegroundColor Gray
Write-Host "POST $configUri body { key=drain_mode, value=on }" -ForegroundColor Gray
if ($ExecuteSteps) {
    $internalHeaders = New-InternalHeaders
    Invoke-JsonPost -Name "old drain reason" -Uri $configUri -Headers $internalHeaders -BodyObject @{ key = "drain_mode_reason"; value = "rollout-drill:$RolloutEpoch" } | Out-Null
    Invoke-JsonPost -Name "old drain source" -Uri $configUri -Headers $internalHeaders -BodyObject @{ key = "drain_mode_source"; value = "scripts/ops/rollout-three-process-drill.ps1" } | Out-Null
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
    "--resolved-control-targets",
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
    "--proxy-admin-actor", $AdminActor,
    "--timeout-ms", [string]$TimeoutMs
)
$transferDryRunArgs = @(
    $TransferCli,
    "--dry-run",
    "--resolved-control-targets",
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
    "--proxy-admin-actor", $AdminActor,
    "--timeout-ms", [string]$TimeoutMs
)
$transferDisplayArgs = @(
    "node",
    "tools/mock-client/src/rollout-transfer-cli.js",
    "--resolved-control-targets",
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
    "--proxy-admin-actor", $AdminActor,
    "--timeout-ms", [string]$TimeoutMs
)
Write-CommandLine $transferDisplayArgs
if ($ExecuteSteps) {
    $transferExecution = Invoke-ExternalJsonStep -Name "room transfer orchestration" -FilePath "node" -Arguments $transferArgs
    $script:TransferCliResult = $transferExecution.json
    if ($transferExecution.exitCode -ne 0 -or -not $transferExecution.json.ok) {
        $stage = if ($transferExecution.json.summary.stage) { $transferExecution.json.summary.stage } else { "unknown" }
        $errorCode = if ($transferExecution.json.summary.errorCode) { $transferExecution.json.summary.errorCode } else { "transfer failed" }
        Add-StageResult "room-transfer" "failed" "$stage $errorCode"
        throw "room transfer orchestration failed with exit code $($transferExecution.exitCode)"
    }
    Add-StageResult "room-transfer" "ok" "stage=$($transferExecution.json.summary.stage)"
} else {
    Write-Host "Transfer dry-run plan:" -ForegroundColor Gray
    $transferDryRun = Invoke-ExternalJsonStep -Name "room transfer dry-run plan" -FilePath "node" -Arguments $transferDryRunArgs
    $script:TransferCliResult = $transferDryRun.json
    if ($transferDryRun.exitCode -ne 0 -or -not $transferDryRun.json.ok) {
        Add-StageResult "room-transfer-dry-run" "failed" "rollout-transfer-cli validation failed"
        Write-RunSummary
        Write-RunReport
        throw "room transfer dry-run plan failed with exit code $($transferDryRun.exitCode)"
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
if ($script:OldProcessManager.enabled) {
    $shutdownDisplayArgs += @(
        "--shutdown-wait-pid", "<old-process-pid>",
        "--shutdown-wait-timeout-ms", [string]$ShutdownWaitTimeoutMs
    )
}
Write-CommandLine $shutdownDisplayArgs

if ($SkipShutdownRequest) {
    Write-Host "Shutdown request skipped by -SkipShutdownRequest." -ForegroundColor Yellow
    Add-StageResult "shutdown-safety-gate" "skipped" "SkipShutdownRequest"
    if ($script:OldProcessManager.enabled) {
        Add-StageResult "old-process-stop" "skipped" "shutdown request skipped"
    }
} elseif (-not $AllowShutdownRequest) {
    Write-Host "Shutdown request is not executed unless -AllowShutdownRequest is passed." -ForegroundColor Yellow
    Add-StageResult "shutdown-safety-gate" "skipped" "requires AllowShutdownRequest"
    if ($script:OldProcessManager.enabled) {
        Add-StageResult "old-process-stop" "skipped" "requires AllowShutdownRequest"
    }
} elseif ($ExecuteSteps) {
    $shutdownArgs = @(
        $MockClientIndex,
        "--scenario", "request-server-shutdown",
        "--http-base-url", $AuthBaseUrl,
        "--shutdown-reason", "rollout-three-process-drill:$RolloutEpoch",
        "--timeout-ms", [string]$TimeoutMs,
        "--json-output"
    ) + (Get-MockClientServiceTokenArgs)
    if ($script:OldProcessManager.enabled) {
        $shutdownArgs += @(
            "--shutdown-wait-pid", [string]$script:OldProcessManager.pid,
            "--shutdown-wait-timeout-ms", [string]$ShutdownWaitTimeoutMs
        )
    }
    $shutdownExecution = Invoke-ExternalJsonStep -Name "old server shutdown safety gate" -FilePath "node" -Arguments $shutdownArgs
    $script:ShutdownResult = $shutdownExecution.json
    if ($shutdownExecution.exitCode -ne 0 -or -not $shutdownExecution.json.ok) {
        $errorCode = if ($shutdownExecution.json.processExit.errorCode) {
            $shutdownExecution.json.processExit.errorCode
        } elseif ($shutdownExecution.json.shutdown.errorCode) {
            $shutdownExecution.json.shutdown.errorCode
        } else {
            "shutdown failed"
        }
        if ($shutdownExecution.json.shutdown.ok) {
            Add-StageResult "shutdown-safety-gate" "ok"
            Add-StageResult "old-process-stop" "failed" $errorCode
        } else {
            Add-StageResult "shutdown-safety-gate" "failed" $errorCode
        }
        throw "old server shutdown safety gate failed with exit code $($shutdownExecution.exitCode)"
    }
    Add-StageResult "shutdown-safety-gate" "ok"
    if ($script:OldProcessManager.enabled) {
        $waitedMs = if ($shutdownExecution.json.processExit.waitedMs -ne $null) { $shutdownExecution.json.processExit.waitedMs } else { 0 }
        Add-StageResult "old-process-stop" "ok" "pid=$($script:OldProcessManager.pid) waitedMs=$waitedMs"
    } else {
        Add-StageResult "old-process-stop" "skipped" "no old process pid"
    }
} else {
    Add-StageResult "shutdown-safety-gate" "planned" "requires ExecuteSteps and AllowShutdownRequest"
    if ($script:OldProcessManager.enabled) {
        Add-StageResult "old-process-stop" "planned" "pid=$($script:OldProcessManager.pid)"
    }
}

Write-RunSummary
Write-RunReport

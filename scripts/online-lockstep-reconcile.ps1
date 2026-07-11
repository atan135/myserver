[CmdletBinding()]
param(
    [Parameter(Mandatory=$false)]
    [ValidateSet("all", "move", "melee", "observer", "single-client", "dual-client", "reconnect-observer", "visual-smoke")]
    [string[]]$Check = @("all"),

    [Parameter(Mandatory=$false)]
    [ValidateSet("lockstep-client", "mybevy")]
    [string]$Client = "lockstep-client",

    [Parameter(Mandatory=$false)]
    [string]$ClientRoot = "",

    [Parameter(Mandatory=$false)]
    [switch]$DryRun,

    [Parameter(Mandatory=$false)]
    [switch]$Execute,

    [Parameter(Mandatory=$false)]
    [switch]$SelfTest,

    [Parameter(Mandatory=$false)]
    [switch]$StartDevStack,

    [Parameter(Mandatory=$false)]
    [switch]$ProvisionDevTickets,

    [Parameter(Mandatory=$false)]
    [switch]$SkipTicketRedisPreflight,

    [Parameter(Mandatory=$false)]
    [string]$Server = "127.0.0.1:7000",

    [Parameter(Mandatory=$false)]
    [string]$RedisUrl = "redis://127.0.0.1:6379",

    [Parameter(Mandatory=$false)]
    [string]$RedisKeyPrefix = "",

    [Parameter(Mandatory=$false)]
    [string]$TicketEnvVar = "MYSERVER_LOCKSTEP_TICKET",

    [Parameter(Mandatory=$false)]
    [string]$ObserverTicketEnvVar = "MYSERVER_LOCKSTEP_OBSERVER_TICKET",

    [Parameter(Mandatory=$false)]
    [string]$TicketSecretEnvVar = "MYSERVER_LOCKSTEP_TICKET_SECRET",

    [Parameter(Mandatory=$false)]
    [string]$TicketSource = "auth-http-external",

    [Parameter(Mandatory=$false)]
    [int]$TicketTtlSeconds = 900,

    [Parameter(Mandatory=$false)]
    [int]$WorldId = 1,

    [Parameter(Mandatory=$false)]
    [int]$GamePort = 7000,

    [Parameter(Mandatory=$false)]
    [int]$GameAdminPort = 7500,

    [Parameter(Mandatory=$false)]
    [int]$TimeoutMs = 5000,

    [Parameter(Mandatory=$false)]
    [string]$RunId = "",

    [Parameter(Mandatory=$false)]
    [string]$ArtifactRoot = ""
)

$ErrorActionPreference = "Stop"
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $ProjectRoot "tools\lockstep-client\Cargo.toml"
$ConfiguredClientRoot = if ($ClientRoot) {
    $ClientRoot
} else {
    $processClientRoot = [Environment]::GetEnvironmentVariable("MYSERVER_CLIENT_ROOT", "Process")
    if ($processClientRoot) { $processClientRoot } else { [Environment]::GetEnvironmentVariable("MYSERVER_CLIENT_ROOT", "User") }
}
$MybevyManifestPath = if ($ConfiguredClientRoot) { Join-Path ([System.IO.Path]::GetFullPath($ConfiguredClientRoot)) "project\Cargo.toml" } else { $null }
$TicketStorePath = Join-Path $ProjectRoot "tools\lockstep-client\online-ticket-store.mjs"
$DevStackPath = Join-Path $ProjectRoot "scripts\dev-stack.ps1"
$DevStackPidFile = Join-Path $ProjectRoot "logs\dev-stack\dev-stack.pids.json"
$RedisRuntimeEnvVar = "MYSERVER_LOCKSTEP_REDIS_URL_RUNTIME"
$ReportSchema = "myserver.lockstep-online-reconcile.report.v1"
$LocalNatsUrl = "nats://127.0.0.1:4222"
$RegistryServiceName = "game-server"
$MybevyVisualSmokeEnvironmentNames = @(
    "TOUCH_START_SCREEN", "MYBEVY_START_SCENE", "LOCKSTEP_SIM_AUTHORITY_MODE",
    "LOCKSTEP_SIM_TRANSPORT", "LOCKSTEP_SIM_MYSERVER_TICKET_ENV",
    "LOCKSTEP_SIM_MYSERVER_ROOM", "LOCKSTEP_SIM_MYSERVER_POLICY",
    "LOCKSTEP_SIM_DEBUG_DIAGNOSTICS", "LOCKSTEP_SIM_VISUAL_SMOKE",
    "LOCKSTEP_SIM_VISUAL_SMOKE_RUN_ID", "LOCKSTEP_SIM_VISUAL_SMOKE_SCREENSHOT",
    "LOCKSTEP_SIM_VISUAL_SMOKE_REPORT", "LOCKSTEP_SIM_VISUAL_SMOKE_OFFLINE_SCREENSHOT",
    "LOCKSTEP_SIM_VISUAL_SMOKE_OFFLINE_REPORT", "LOCKSTEP_SIM_VISUAL_SMOKE_TIMEOUT_MS",
    "MYSERVER_TRANSPORT", "MYSERVER_GAME_HOST", "MYSERVER_TCP_FALLBACK_PORT",
    "MYSERVER_REQUEST_TIMEOUT_MS"
)

function Get-NowIso {
    return (Get-Date).ToUniversalTime().ToString("o")
}

function New-RunId {
    $stamp = (Get-Date).ToUniversalTime().ToString("yyyyMMdd-HHmmss")
    $suffix = [Guid]::NewGuid().ToString("N").Substring(0, 8)
    return "$stamp-$suffix".ToLowerInvariant()
}

function New-EphemeralTicketSecret {
    $bytes = New-Object byte[] 32
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $rng.GetBytes($bytes)
    } finally {
        $rng.Dispose()
    }
    return [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
}

function Assert-EnvironmentVariableName {
    param([string]$Name, [string]$ParameterName)
    if ($Name -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') {
        throw "$ParameterName must be a valid environment variable name."
    }
}

function Get-RedactedRedisEndpoint {
    param([string]$Value)
    $uri = [Uri]$Value
    $endpointHost = $uri.Host.ToLowerInvariant()
    if ($endpointHost.Contains(":")) { $endpointHost = "[$endpointHost]" }
    $port = if ($uri.Port -gt 0) { $uri.Port } else { 6379 }
    $database = $uri.AbsolutePath.Trim("/")
    $path = if ($database) { "/$database" } else { "/0" }
    return "$($uri.Scheme.ToLowerInvariant())://${endpointHost}:$port$path"
}

function New-RegistryOwnership {
    $instanceId = "lockstep-$RunId"
    return [ordered]@{
        status = "planned"
        serviceName = $RegistryServiceName
        instanceId = $instanceId
        gameInstanceIdArgument = $instanceId
        instanceKey = "$($RedisKeyPrefix)service:${RegistryServiceName}:instances:$instanceId"
        heartbeatKey = "$($RedisKeyPrefix)heartbeat:${RegistryServiceName}:$instanceId"
        expectedInstanceHash = [ordered]@{ id = $instanceId; name = $RegistryServiceName }
        expectedHeartbeatValue = "1"
        startInvocationAt = $null
        ownedAt = $null
        cleanedAt = $null
        confirmedGameServer = $null
    }
}

function Test-RegistryCleanupOwnership {
    param(
        [AllowNull()][object]$RegistryOwnership,
        [object[]]$OwnedServices,
        [string]$ExpectedInstanceId
    )
    if (-not $RegistryOwnership -or $RegistryOwnership.status -ne "owned") { return $false }
    if ($RegistryOwnership.serviceName -ne $RegistryServiceName -or $RegistryOwnership.instanceId -ne $ExpectedInstanceId) { return $false }

    $ownedGameServices = @($OwnedServices | Where-Object { $_.name -eq "game-server" })
    if ($ownedGameServices.Count -ne 1 -or -not $RegistryOwnership.confirmedGameServer) { return $false }

    $ownedGameServer = $ownedGameServices[0]
    $confirmedGameServer = $RegistryOwnership.confirmedGameServer
    return [bool](
        [int]$confirmedGameServer.pid -eq [int]$ownedGameServer.pid -and
        [long]$confirmedGameServer.startTimeUtcTicks -gt 0 -and
        [long]$confirmedGameServer.startTimeUtcTicks -eq [long]$ownedGameServer.startTimeUtcTicks -and
        [string]$confirmedGameServer.startedAt -eq [string]$ownedGameServer.startedAt
    )
}

function Assert-RunOptions {
    param([string]$Mode, [string[]]$Checks)

    if ($Mode -eq "dry-run" -and ($StartDevStack -or $ProvisionDevTickets)) {
        throw "-DryRun cannot be combined with -StartDevStack or -ProvisionDevTickets."
    }
    if ($ProvisionDevTickets -and $SkipTicketRedisPreflight) {
        throw "-ProvisionDevTickets cannot be combined with -SkipTicketRedisPreflight."
    }
    if ($RunId -notmatch '^[a-z0-9][a-z0-9-]{2,39}$') {
        throw "-RunId must contain 3-40 lowercase letters, digits, or hyphens."
    }
    if ($Server -notmatch '^(localhost|127\.0\.0\.1):([1-9][0-9]{0,4})$') {
        throw "-Server must be an explicit loopback host:port for this local dev tool."
    }
    $serverPort = [int]$Matches[2]
    if ($serverPort -gt 65535) {
        throw "-Server port must be at most 65535."
    }
    if ($StartDevStack -and $serverPort -ne $GamePort) {
        throw "When -StartDevStack is used, -Server port must equal -GamePort."
    }
    try {
        $redisUri = [Uri]$RedisUrl
    } catch {
        throw "-RedisUrl must be a valid redis:// or rediss:// URI."
    }
    if ($redisUri.Scheme -notin @("redis", "rediss") -or $redisUri.Host -notin @("127.0.0.1", "localhost", "::1")) {
        throw "-RedisUrl must identify loopback Redis with redis:// or rediss://."
    }
    if ($redisUri.AbsolutePath -notmatch '^(?:/|/[0-9]+)?$') {
        throw "-RedisUrl database path must be empty or a numeric Redis database."
    }
    if ($StartDevStack -and ($redisUri.Scheme -ne "redis" -or $redisUri.Port -ne 6379 -or $redisUri.UserInfo -or $redisUri.AbsolutePath -notin @("", "/", "/0") -or $redisUri.Query -or $redisUri.Fragment)) {
        throw "-StartDevStack uses unauthenticated local Redis database 0 at port 6379; adjust -RedisUrl or manage Redis externally."
    }
    if ($GamePort -lt 1 -or $GamePort -gt 65535 -or $GameAdminPort -lt 1 -or $GameAdminPort -gt 65535) {
        throw "Game ports must be from 1 through 65535."
    }
    if ($GamePort -eq $GameAdminPort) {
        throw "-GamePort and -GameAdminPort must differ."
    }
    if ($TimeoutMs -lt 1 -or $TimeoutMs -gt 300000) {
        throw "-TimeoutMs must be from 1 through 300000."
    }
    if ($TicketTtlSeconds -lt 30 -or $TicketTtlSeconds -gt 3600) {
        throw "-TicketTtlSeconds must be from 30 through 3600."
    }
    if ($WorldId -lt 0) {
        throw "-WorldId must be non-negative."
    }
    if ($RedisKeyPrefix -match '[*?\[\]\x00\r\n]') {
        throw "-RedisKeyPrefix cannot contain wildcard or control characters."
    }
    if ($TicketSource -notmatch '^[A-Za-z0-9._:-]{1,64}$') {
        throw "-TicketSource contains unsupported characters."
    }
    Assert-EnvironmentVariableName -Name $TicketEnvVar -ParameterName "-TicketEnvVar"
    Assert-EnvironmentVariableName -Name $ObserverTicketEnvVar -ParameterName "-ObserverTicketEnvVar"
    Assert-EnvironmentVariableName -Name $TicketSecretEnvVar -ParameterName "-TicketSecretEnvVar"
    if ($TicketEnvVar -eq $ObserverTicketEnvVar) {
        throw "Primary and observer ticket environment variables must differ."
    }
    $ticketEnvironmentNames = @($TicketEnvVar, $ObserverTicketEnvVar, $TicketSecretEnvVar)
    if (@($ticketEnvironmentNames | Select-Object -Unique).Count -ne 3) {
        throw "Ticket, observer ticket, and ticket secret environment variables must be distinct."
    }
    $reservedEnvironmentNames = @(
        "TICKET_SECRET", $RedisRuntimeEnvVar, "REDIS_URL", "REDIS_KEY_PREFIX",
        "REGISTRY_URL", "REGISTRY_KEY_PREFIX", "NATS_URL", "DB_ENABLED", "SERVICE_NAME"
    ) + $MybevyVisualSmokeEnvironmentNames
    foreach ($name in $ticketEnvironmentNames) {
        if ($reservedEnvironmentNames -contains $name) {
            throw "Ticket environment variable $name aliases a reserved runtime variable."
        }
    }
    if ($Checks.Count -eq 0) {
        throw "At least one check is required."
    }
    if ($Client -eq "mybevy") {
        if (@($Checks | Where-Object { $_ -notin @("single-client", "dual-client", "reconnect-observer", "visual-smoke") }).Count -gt 0) {
            throw "-Client mybevy only supports -Check single-client, dual-client, reconnect-observer, visual-smoke (or -Check all)."
        }
        if (-not $ConfiguredClientRoot) {
            throw "-Client mybevy requires -ClientRoot or MYSERVER_CLIENT_ROOT."
        }
        if (-not (Test-Path -LiteralPath $MybevyManifestPath -PathType Leaf)) {
            throw "mybevy Cargo manifest not found under the configured client root."
        }
    } elseif (@($Checks | Where-Object { $_ -in @("single-client", "dual-client", "reconnect-observer", "visual-smoke") }).Count -gt 0) {
        throw "-Check single-client, dual-client, reconnect-observer, and visual-smoke require -Client mybevy."
    }
}

function Get-NormalizedChecks {
    $requested = @($Check | ForEach-Object { $_.ToLowerInvariant() })
    if ($requested -contains "all") {
        if ($Client -eq "mybevy") {
            return @("single-client", "dual-client", "reconnect-observer", "visual-smoke")
        }
        return @("move", "melee", "observer")
    }
    return @($requested | Select-Object -Unique)
}

function New-StageDefinitions {
    param([string[]]$Checks, [string]$CurrentRunId)
    $definitions = @()
    foreach ($name in $Checks) {
        switch ($name) {
            "move" {
                $definitions += [pscustomobject]@{
                    name = "move"
                    scenario = "move_straight"
                    roomId = "lockstep-$CurrentRunId-move"
                    observerProbe = $false
                }
            }
            "melee" {
                $definitions += [pscustomobject]@{
                    name = "melee"
                    scenario = "lockstep_demo_melee"
                    roomId = "lockstep-$CurrentRunId-melee"
                    observerProbe = $false
                }
            }
            "observer" {
                $definitions += [pscustomobject]@{
                    name = "observer-recovery"
                    scenario = "move_straight"
                    roomId = "lockstep-$CurrentRunId-observer"
                    observerProbe = $true
                }
            }
            "single-client" {
                $definitions += [pscustomobject]@{
                    name = "mybevy-single-client"
                    scenario = "online-single-client"
                    roomId = "lockstep-$CurrentRunId-mybevy"
                    observerProbe = $false
                    visualSmoke = $false
                    dualClient = $false
                    reconnectObserver = $false
                }
            }
            "dual-client" {
                $definitions += [pscustomobject]@{
                    name = "mybevy-dual-client"
                    scenario = "online-dual-client"
                    roomId = "lockstep-$CurrentRunId-mybevy-dual"
                    observerProbe = $false
                    visualSmoke = $false
                    dualClient = $true
                    reconnectObserver = $false
                }
            }
            "reconnect-observer" {
                $definitions += [pscustomobject]@{
                    name = "mybevy-reconnect-observer"
                    scenario = "online-reconnect-observer"
                    roomId = "lockstep-$CurrentRunId-mybevy-recovery"
                    observerProbe = $false
                    visualSmoke = $false
                    dualClient = $false
                    reconnectObserver = $true
                }
            }
            "visual-smoke" {
                $definitions += [pscustomobject]@{
                    name = "mybevy-visual-smoke"
                    scenario = "gui-visual-smoke"
                    roomId = "lockstep-$CurrentRunId-mybevy-gui"
                    observerProbe = $false
                    visualSmoke = $true
                    dualClient = $false
                    reconnectObserver = $false
                }
            }
            default { throw "Unsupported check: $name" }
        }
    }
    return @($definitions)
}

function New-ClientArguments {
    param([pscustomobject]$Stage, [string]$Mode)
    if ($Client -eq "mybevy") {
        if ($Mode -eq "dry-run") {
            return [string[]]@(
                "run", "--quiet",
                "--manifest-path", $MybevyManifestPath,
                "--bin", "lockstep-sim-headless",
                "--",
                "--scenario", "offline-fixture",
                "--run-id", $RunId,
                "--room", $Stage.roomId,
                "--policy", "lockstep_sim_demo"
            )
        }
        if ($Stage.visualSmoke) {
            return [string[]]@(
                "run", "--quiet",
                "--manifest-path", $MybevyManifestPath,
                "--bin", "project",
                "--",
                "--window-profile", "desktop"
            )
        }
        $scenario = if ($Stage.reconnectObserver) {
            "online-reconnect-observer"
        } elseif ($Stage.dualClient) {
            "online-dual-client"
        } else {
            "online-single-client"
        }
        $arguments = @(
            "run", "--quiet",
            "--manifest-path", $MybevyManifestPath,
            "--bin", "lockstep-sim-headless",
            "--",
            "--scenario", $scenario,
            "--run-id", $RunId,
            "--room", $Stage.roomId,
            "--policy", "lockstep_sim_demo",
            "--endpoint", $Server,
            "--connect-timeout-ms", [string]$TimeoutMs,
            "--ticket-env", $TicketEnvVar
        )
        if ($Stage.dualClient -or $Stage.reconnectObserver) {
            $arguments += @("--observer-ticket-env", $ObserverTicketEnvVar)
        }
        return [string[]]$arguments
    }
    $arguments = @(
        "run", "--quiet",
        "--manifest-path", $ManifestPath,
        "--",
        "--mode", "online",
        "--scenario", $Stage.scenario,
        "--server", $Server,
        "--room", $Stage.roomId,
        "--policy", "lockstep_sim_demo",
        "--timeout-ms", [string]$TimeoutMs
    )
    if ($Stage.observerProbe) {
        $arguments += "--probe-observer-recovery"
    }
    if ($Mode -eq "dry-run") {
        $arguments += "--dry-run"
    } elseif ($Mode -eq "execute") {
        $arguments += @("--ticket-env", $TicketEnvVar)
        if ($Stage.observerProbe) {
            $arguments += @("--observer-ticket-env", $ObserverTicketEnvVar)
        }
    }
    return [string[]]$arguments
}

function Format-Command {
    param([string]$Executable, [string[]]$Arguments)
    $parts = @($Executable)
    foreach ($argument in $Arguments) {
        if ($argument -match '[\s'']') {
            $parts += "'" + $argument.Replace("'", "''") + "'"
        } else {
            $parts += $argument
        }
    }
    return ($parts -join " ")
}

function New-DependencyReport {
    return @(
        [pscustomobject]@{ name = "Redis"; required = $true; endpoint = Get-RedactedRedisEndpoint -Value $RedisUrl; defaultPort = 6379; ownership = "external-or-reused"; purpose = "ticket/session keys and service registry" },
        [pscustomobject]@{ name = "Core NATS"; required = $true; endpoint = $LocalNatsUrl; defaultPort = 4222; ownership = "external-or-reused"; purpose = "game-server runtime channels" },
        [pscustomobject]@{ name = "game-server"; required = $true; endpoint = $Server; defaultPort = $GamePort; ownership = if ($StartDevStack) { "must-start-by-run" } else { "operator-owned" }; purpose = "local direct online reconciliation" },
        [pscustomobject]@{ name = "game-server-admin"; required = $true; endpoint = "127.0.0.1:$GameAdminPort"; defaultPort = $GameAdminPort; ownership = if ($StartDevStack) { "must-start-by-run" } else { "operator-owned" }; purpose = "dev-stack readiness only" },
        [pscustomobject]@{ name = "auth-http"; required = $false; endpoint = "http://127.0.0.1:3000"; defaultPort = 3000; ownership = "not-started"; purpose = "issue external character tickets before this run" },
        [pscustomobject]@{ name = "PostgreSQL"; required = $false; endpoint = "not accessed by wrapper"; defaultPort = 5432; ownership = if ($StartDevStack) { "disabled" } else { "operator-owned-config" }; purpose = if ($StartDevStack) { "DB_ENABLED=false is forced for wrapper-owned game-server" } else { "external endpoint database behavior is operator-owned" } }
    )
}

function New-RunReport {
    param([string]$Mode, [pscustomobject[]]$Definitions, [string]$ArtifactDirectory)
    $commands = @()
    $commandMode = if ($Mode -eq "plan") { "execute" } else { $Mode }
    foreach ($definition in $Definitions) {
        $commands += [pscustomobject]@{
            stage = $definition.name
            command = Format-Command -Executable "cargo" -Arguments (New-ClientArguments -Stage $definition -Mode $commandMode)
            containsTicketValue = $false
        }
    }
    return [ordered]@{
        schema = $ReportSchema
        schemaVersion = 1
        runId = $RunId
        client = [ordered]@{
            kind = $Client
            root = if ($Client -eq "mybevy") { [System.IO.Path]::GetFullPath($ConfiguredClientRoot) } else { $null }
            manifest = if ($Client -eq "mybevy") { $MybevyManifestPath } else { $ManifestPath }
        }
        mode = $Mode
        status = if ($Mode -eq "plan") { "planned" } else { "running" }
        startedAt = Get-NowIso
        endedAt = $null
        sideEffects = ($Mode -ne "plan")
        externalSideEffects = ($Mode -eq "execute")
        writesArtifacts = ($Mode -ne "plan")
        networkConnectionsAllowed = ($Mode -eq "execute")
        endpoint = [ordered]@{
            value = $Server
            transport = "local TCP direct debug endpoint"
            discoveryBoundary = "Fixed ports are local-only; test/staging/production must use service registry discovery."
        }
        ticket = [ordered]@{
            source = if ($ProvisionDevTickets) { "generated-dev-redis" } else { $TicketSource }
            valueRecorded = $false
            primaryEnvVar = $TicketEnvVar
            observerEnvVar = $ObserverTicketEnvVar
            secretEnvVar = if ($ProvisionDevTickets) { $TicketSecretEnvVar } else { $null }
            ephemeralSecretGenerated = $false
            primaryFingerprint = $null
            observerFingerprint = $null
            signatureVerifiedByScript = $false
            redisBindingsVerified = $false
            validatedRedisKeys = @()
            ownedRedisKeys = @()
        }
        runtimeConfig = [ordered]@{
            ownership = if ($StartDevStack) { "wrapper-owned" } else { "operator-owned" }
            redisEndpoint = Get-RedactedRedisEndpoint -Value $RedisUrl
            redisKeyPrefix = $RedisKeyPrefix
            registryEndpoint = Get-RedactedRedisEndpoint -Value $RedisUrl
            registryKeyPrefix = $RedisKeyPrefix
            natsEndpoint = $LocalNatsUrl
            dbEnabled = if ($StartDevStack) { $false } else { $null }
            postgresTouchedByWrapper = $false
        }
        rooms = @($Definitions | ForEach-Object { $_.roomId })
        dependencies = @(New-DependencyReport)
        commands = $commands
        stages = @()
        ownership = [ordered]@{
            startRequested = [bool]$StartDevStack
            services = @()
            registry = if ($StartDevStack) { New-RegistryOwnership } else { $null }
            reusedServicesAreStopped = $false
        }
        cleanup = [ordered]@{
            attempted = $false
            redis = [ordered]@{ attempted = $false; ok = $true; results = @() }
            registry = [ordered]@{ attempted = $false; ok = $true; reason = $null; results = @(); guardCode = $null }
            processes = [ordered]@{ attempted = $false; ok = $true; results = @() }
            ports = @()
            pidFile = [ordered]@{ path = $DevStackPidFile; removed = $false; reason = $null }
            environment = [ordered]@{ attempted = $false; ok = $true; errors = @() }
            errors = @()
            ok = $true
        }
        logs = [ordered]@{
            artifactDirectory = $ArtifactDirectory
            report = if ($ArtifactDirectory) { Join-Path $ArtifactDirectory "report.json" } else { $null }
            devStackDirectory = Join-Path $ProjectRoot "logs\dev-stack"
            gameServerDirectory = Join-Path $ProjectRoot "logs\game-server"
        }
        failure = $null
    }
}

function Save-RunReport {
    param([System.Collections.IDictionary]$Report)
    if (-not $Report.logs.report) { return }
    $Report | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath $Report.logs.report -Encoding UTF8
}

function Add-CleanupError {
    param(
        [System.Collections.IDictionary]$Report,
        [string]$Stage,
        [string]$Message
    )
    $Report.cleanup.ok = $false
    $Report.cleanup.errors += [pscustomobject]@{ stage = $Stage; message = $Message }
}

function Get-EnvironmentValue {
    param([string]$Name)
    return [Environment]::GetEnvironmentVariable($Name, "Process")
}

function Set-ProcessEnvironmentValue {
    param([string]$Name, [AllowNull()][string]$Value)
    [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
}

function Get-MybevyVisualSmokeArtifactPaths {
    param([string]$ArtifactDirectory)
    return [ordered]@{
        onlineScreenshot = Join-Path $ArtifactDirectory "mybevy-online.png"
        onlineReport = Join-Path $ArtifactDirectory "mybevy-online-report.json"
        offlineScreenshot = Join-Path $ArtifactDirectory "mybevy-offline-fixture.png"
        offlineReport = Join-Path $ArtifactDirectory "offline-fixture-report.json"
    }
}

function Set-MybevyVisualSmokeEnvironment {
    param([pscustomobject]$Definition, [string]$ArtifactDirectory)

    $paths = Get-MybevyVisualSmokeArtifactPaths -ArtifactDirectory $ArtifactDirectory
    if ($Server -notmatch '^(localhost|127\.0\.0\.1):([1-9][0-9]{0,4})$') {
        throw "visual smoke requires an explicit loopback TCP endpoint"
    }
    $gameHost = $Matches[1]
    $gamePort = $Matches[2]
    $visualTimeoutMs = [Math]::Max(30000, $TimeoutMs)
    $values = [ordered]@{
        TOUCH_START_SCREEN = "robot_sync_scene"
        MYBEVY_START_SCENE = "arena.lockstep_sim"
        LOCKSTEP_SIM_AUTHORITY_MODE = "myserver"
        LOCKSTEP_SIM_TRANSPORT = "tcp"
        LOCKSTEP_SIM_MYSERVER_TICKET_ENV = $TicketEnvVar
        LOCKSTEP_SIM_MYSERVER_ROOM = [string]$Definition.roomId
        LOCKSTEP_SIM_MYSERVER_POLICY = "lockstep_sim_demo"
        LOCKSTEP_SIM_DEBUG_DIAGNOSTICS = "1"
        LOCKSTEP_SIM_VISUAL_SMOKE = "1"
        LOCKSTEP_SIM_VISUAL_SMOKE_RUN_ID = $RunId
        LOCKSTEP_SIM_VISUAL_SMOKE_SCREENSHOT = [string]$paths.onlineScreenshot
        LOCKSTEP_SIM_VISUAL_SMOKE_REPORT = [string]$paths.onlineReport
        LOCKSTEP_SIM_VISUAL_SMOKE_OFFLINE_SCREENSHOT = [string]$paths.offlineScreenshot
        LOCKSTEP_SIM_VISUAL_SMOKE_OFFLINE_REPORT = [string]$paths.offlineReport
        LOCKSTEP_SIM_VISUAL_SMOKE_TIMEOUT_MS = [string]$visualTimeoutMs
        MYSERVER_TRANSPORT = "tcp"
        MYSERVER_GAME_HOST = [string]$gameHost
        MYSERVER_TCP_FALLBACK_PORT = [string]$gamePort
        MYSERVER_REQUEST_TIMEOUT_MS = [string]$TimeoutMs
    }
    foreach ($entry in $values.GetEnumerator()) {
        Set-ProcessEnvironmentValue -Name $entry.Key -Value ([string]$entry.Value)
    }
    return $paths
}

function Read-JsonArtifact {
    param([string]$Path, [string]$Label)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }
    try {
        return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    } catch {
        throw "$Label is not valid JSON: $Path"
    }
}

function Invoke-TicketStore {
    param([hashtable]$Request)
    $requestJson = $Request | ConvertTo-Json -Depth 12 -Compress
    $output = $requestJson | & node $TicketStorePath 2>&1
    $exitCode = $LASTEXITCODE
    $text = ($output | Out-String).Trim()
    if ($exitCode -ne 0) {
        throw "ticket store helper failed: $text"
    }
    try {
        return $text | ConvertFrom-Json
    } catch {
        throw "ticket store helper returned invalid JSON"
    }
}

function ConvertTo-NativeCommandLineArgument {
    param([AllowEmptyString()][string]$Argument)

    if ($null -eq $Argument) { $Argument = "" }
    if ($Argument.Length -gt 0 -and $Argument -notmatch '[\s"]') { return $Argument }

    # Preserve Windows CommandLineToArgvW semantics for spaces, quotes, and trailing slashes.
    $builder = New-Object System.Text.StringBuilder
    [void]$builder.Append('"')
    $backslashCount = 0
    foreach ($character in $Argument.ToCharArray()) {
        if ($character -eq '\') {
            $backslashCount++
            continue
        }
        if ($character -eq '"') {
            for ($index = 0; $index -lt (($backslashCount * 2) + 1); $index++) {
                [void]$builder.Append('\')
            }
            [void]$builder.Append('"')
            $backslashCount = 0
            continue
        }
        for ($index = 0; $index -lt $backslashCount; $index++) {
            [void]$builder.Append('\')
        }
        $backslashCount = 0
        [void]$builder.Append($character)
    }
    for ($index = 0; $index -lt ($backslashCount * 2); $index++) {
        [void]$builder.Append('\')
    }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Get-NativeProcessExitCode {
    param(
        [IntPtr]$ProcessHandle,
        [int]$ProcessId
    )

    if (-not ("MyServer.NativeProcessMethods" -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace MyServer
{
    public static class NativeProcessMethods
    {
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetExitCodeProcess(IntPtr processHandle, out uint exitCode);
    }
}
'@
    }

    [uint32]$exitCode = 0
    if (-not [MyServer.NativeProcessMethods]::GetExitCodeProcess($ProcessHandle, [ref]$exitCode)) {
        $nativeError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "failed to read native launcher PID $ProcessId exit code (Win32 error $nativeError)"
    }
    return [BitConverter]::ToInt32([BitConverter]::GetBytes($exitCode), 0)
}

function Invoke-NativeCaptured {
    param(
        [string]$FilePath,
        [string[]]$Arguments,
        [string]$StdoutPath,
        [string]$StderrPath,
        [string]$WorkingDirectory = $ProjectRoot
    )

    $argumentLine = (@($Arguments | ForEach-Object {
        ConvertTo-NativeCommandLineArgument -Argument ([string]$_)
    }) -join " ")
    $startParameters = @{
        FilePath = $FilePath
        WorkingDirectory = $WorkingDirectory
        WindowStyle = "Hidden"
        RedirectStandardOutput = $StdoutPath
        RedirectStandardError = $StderrPath
        PassThru = $true
    }
    if ($argumentLine) { $startParameters["ArgumentList"] = $argumentLine }

    $launcher = Start-Process @startParameters
    try {
        $launcherHandle = $launcher.Handle
        $launcherStartTimeUtcTicks = $launcher.StartTime.ToUniversalTime().Ticks
        while ($true) {
            $launcher.Refresh()
            if ($launcher.HasExited) {
                return Get-NativeProcessExitCode -ProcessHandle $launcherHandle -ProcessId $launcher.Id
            }

            $liveLauncher = Get-Process -Id $launcher.Id -ErrorAction SilentlyContinue
            if (-not $liveLauncher) {
                $launcher.Refresh()
                if ($launcher.HasExited) {
                    return Get-NativeProcessExitCode -ProcessHandle $launcherHandle -ProcessId $launcher.Id
                }
                throw "native launcher PID $($launcher.Id) disappeared before its exit code was available"
            }
            if ($liveLauncher.StartTime.ToUniversalTime().Ticks -ne $launcherStartTimeUtcTicks) {
                throw "native launcher PID $($launcher.Id) identity changed while waiting"
            }
            Start-Sleep -Milliseconds 100
        }
    } finally {
        $launcher.Dispose()
    }
}

function Assert-DevStackPidFileCreated {
    param(
        [string]$Path,
        [int]$ExitCode,
        [string]$StdoutPath,
        [string]$StderrPath
    )
    if (Test-Path -LiteralPath $Path) { return }
    if ($ExitCode -ne 0) {
        throw "minimal dev-stack startup failed with exit code $ExitCode and created no PID ownership file; see $StdoutPath and $StderrPath"
    }
    throw "minimal dev-stack returned success without creating the required PID ownership file: $Path"
}

function Read-DevStackPidRecords {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        throw "dev-stack PID ownership file does not exist: $Path"
    }
    $raw = Get-Content -LiteralPath $Path -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) {
        throw "dev-stack PID ownership file is empty: $Path"
    }
    $trimmed = $raw.Trim()
    if (-not $trimmed.StartsWith("[")) {
        throw "dev-stack PID ownership file root must be a JSON array: $Path"
    }
    try {
        $decoded = $raw | ConvertFrom-Json
    } catch {
        throw "dev-stack PID ownership file is not valid JSON: $Path ($($_.Exception.Message))"
    }

    $records = @()
    if ($decoded -is [System.Array]) {
        foreach ($record in $decoded) { $records += $record }
    } elseif ($null -ne $decoded) {
        $records += $decoded
    }
    if ($records.Count -eq 0) {
        throw "dev-stack PID ownership file must contain at least one record: $Path"
    }
    for ($index = 0; $index -lt $records.Count; $index++) {
        $record = $records[$index]
        $pidValue = 0
        if (-not $record -or
            [string]::IsNullOrWhiteSpace([string]$record.name) -or
            -not [int]::TryParse([string]$record.pid, [ref]$pidValue) -or
            $pidValue -le 0 -or
            [string]::IsNullOrWhiteSpace([string]$record.startedAt)) {
            throw "dev-stack PID ownership record $index must contain name, positive pid, and startedAt: $Path"
        }
    }
    return $records
}

function Start-OwnedDevStack {
    param(
        [string]$ArtifactDirectory,
        [Parameter(Mandatory=$true)]
        [DateTime]$InvocationStartedAt
    )
    if (Test-Path -LiteralPath $DevStackPidFile) {
        throw "Refusing to start: $DevStackPidFile already exists. Inspect the existing dev stack first."
    }
    if ((Test-LocalPortListening -Port $GamePort) -or (Test-LocalPortListening -Port $GameAdminPort)) {
        throw "Refusing to reuse game-server: player/admin ports must both be free before -StartDevStack."
    }

    $powerShellHost = (Get-Process -Id $PID).Path
    $stdoutPath = Join-Path $ArtifactDirectory "dev-stack.stdout.log"
    $stderrPath = Join-Path $ArtifactDirectory "dev-stack.stderr.log"
    $arguments = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $DevStackPath,
        "-NoAuth", "-NoProxy", "-NoAdminApi", "-NoAdminWeb", "-NoMetricsCollector",
        "-GamePort", [string]$GamePort,
        "-GameAdminPort", [string]$GameAdminPort,
        "-GameInstanceId", "lockstep-$RunId"
    )
    $ownershipTimeFloor = $InvocationStartedAt.ToUniversalTime().AddSeconds(-2)
    $exitCode = Invoke-NativeCaptured -FilePath $powerShellHost -Arguments $arguments -StdoutPath $stdoutPath -StderrPath $stderrPath
    Assert-DevStackPidFileCreated -Path $DevStackPidFile -ExitCode $exitCode -StdoutPath $stdoutPath -StderrPath $stderrPath
    $items = @(Read-DevStackPidRecords -Path $DevStackPidFile)
    $owned = @()
    foreach ($item in $items) {
        $recordedAt = [DateTime]::Parse([string]$item.startedAt).ToUniversalTime()
        if ($recordedAt -lt $ownershipTimeFloor) {
            throw "dev-stack PID file contains ownership data older than this run; refusing to claim it"
        }
        $process = Get-Process -Id ([int]$item.pid) -ErrorAction SilentlyContinue
        if (-not $process -and $exitCode -eq 0) {
            throw "dev-stack ownership PID $($item.pid) exited before ownership was recorded"
        }
        if ($process -and $process.StartTime.ToUniversalTime() -gt $recordedAt.AddSeconds(2)) {
            throw "dev-stack ownership PID $($item.pid) appears to have been reused; refusing to claim it"
        }
        if ($process -and $process.StartTime.ToUniversalTime() -lt $ownershipTimeFloor) {
            throw "dev-stack ownership PID $($item.pid) started before this invocation; refusing to claim it"
        }
        $owned += [pscustomobject]@{
            name = [string]$item.name
            pid = [int]$item.pid
            startTimeUtcTicks = if ($process) { $process.StartTime.ToUniversalTime().Ticks } else { 0 }
            startedAt = [string]$item.startedAt
            stdout = [string]$item.stdout
            stderr = [string]$item.stderr
        }
        $script:ownedServices = @($owned)
    }
    if ($exitCode -ne 0) {
        if ($owned.Count -eq 0) {
            Remove-Item -LiteralPath $DevStackPidFile -Force -ErrorAction SilentlyContinue
        }
        throw "minimal dev-stack startup failed with exit code $exitCode; see $stdoutPath and $stderrPath"
    }
    if (@($owned | Where-Object { $_.name -eq "game-server" }).Count -ne 1) {
        throw "minimal dev-stack did not return exactly one run-owned game-server process"
    }
    return @($owned)
}

function Get-ChildProcessIds {
    param([int]$ParentProcessId)
    $ids = @()
    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$ParentProcessId" -ErrorAction SilentlyContinue)
    foreach ($child in $children) {
        $ids += Get-ChildProcessIds -ParentProcessId ([int]$child.ProcessId)
        $ids += [int]$child.ProcessId
    }
    return @($ids)
}

function Stop-OwnedProcesses {
    param([pscustomobject[]]$OwnedServices)
    $results = @()
    $reversed = @($OwnedServices)
    [array]::Reverse($reversed)
    foreach ($owned in $reversed) {
        $process = Get-Process -Id $owned.pid -ErrorAction SilentlyContinue
        if (-not $process) {
            $results += [pscustomobject]@{ name = $owned.name; pid = $owned.pid; result = "already-stopped" }
            continue
        }
        if ($process.StartTime.ToUniversalTime().Ticks -ne [long]$owned.startTimeUtcTicks) {
            $results += [pscustomobject]@{ name = $owned.name; pid = $owned.pid; result = "pid-reused-not-stopped" }
            continue
        }
        foreach ($childId in @(Get-ChildProcessIds -ParentProcessId $owned.pid)) {
            Stop-Process -Id $childId -Force -ErrorAction SilentlyContinue
        }
        Stop-Process -Id $owned.pid -Force -ErrorAction SilentlyContinue
        $deadline = (Get-Date).AddSeconds(10)
        while ((Get-Process -Id $owned.pid -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 200
        }
        $result = if (Get-Process -Id $owned.pid -ErrorAction SilentlyContinue) { "stop-timeout" } else { "stopped" }
        $results += [pscustomobject]@{ name = $owned.name; pid = $owned.pid; result = $result }
    }
    return @($results)
}

function Test-LocalPortListening {
    param([int]$Port)
    return $null -ne (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1)
}

function Get-OwnedPortChecks {
    param([pscustomobject[]]$OwnedServices)
    $checks = @()
    foreach ($owned in $OwnedServices) {
        $ports = @()
        switch ($owned.name) {
            "redis" { $ports = @(6379) }
            "nats" { $ports = @(4222) }
            "game-server" { $ports = @($GamePort, $GameAdminPort) }
        }
        foreach ($port in $ports) {
            $checks += [pscustomobject]@{
                service = $owned.name
                port = $port
                listeningAfterCleanup = [bool](Test-LocalPortListening -Port $port)
            }
        }
    }
    return @($checks)
}

function Get-PidOwnershipIdentities {
    param([object[]]$Items)
    return @($Items | ForEach-Object {
        $startedAtTicks = [DateTime]::Parse([string]$_.startedAt).ToUniversalTime().Ticks
        "$($_.name)|$([int]$_.pid)|$startedAtTicks"
    } | Sort-Object)
}

function Remove-OwnedPidFile {
    param([pscustomobject[]]$OwnedServices)
    if (-not (Test-Path -LiteralPath $DevStackPidFile)) {
        return [pscustomobject]@{ removed = $false; reason = "not-present" }
    }
    $current = @(Read-DevStackPidRecords -Path $DevStackPidFile)
    $expectedIdentities = @(Get-PidOwnershipIdentities -Items $OwnedServices)
    $currentIdentities = @(Get-PidOwnershipIdentities -Items $current)
    if (($expectedIdentities -join ",") -ne ($currentIdentities -join ",")) {
        return [pscustomobject]@{ removed = $false; reason = "ownership-changed" }
    }
    Remove-Item -LiteralPath $DevStackPidFile -Force
    return [pscustomobject]@{ removed = $true; reason = "matched-owned-pids" }
}

function Get-TextSection {
    param([string]$Text, [string]$Start, [string]$End)
    $pattern = "(?ms)^" + [regex]::Escape($Start) + "\s*(.*?)(?=^" + $End + "|\z)"
    $match = [regex]::Match($Text, $pattern)
    if (-not $match.Success) { return $null }
    $value = $match.Groups[1].Value.Trim()
    if ($value.Length -gt 4000) { return $value.Substring(0, 4000) }
    return $value
}

function Get-ClientDiagnostics {
    param([string]$Stdout, [string]$Stderr)
    $text = "$Stdout`n$Stderr"
    $frame = $null
    $serverHash = $null
    $clientHash = $null
    $failureStage = $null
    if ($text -match '(?m)first mismatch frame ([0-9]+)') { $frame = [int]$Matches[1] }
    elseif ($text -match '(?m)client replay failed at frame ([0-9]+)') { $frame = [int]$Matches[1] }
    if ($text -match '(?m)^server_hash:\s*([0-9a-fA-F]+)') { $serverHash = $Matches[1].ToLowerInvariant() }
    if ($text -match '(?m)^client_hash:\s*([0-9a-fA-F]+)') { $clientHash = $Matches[1].ToLowerInvariant() }
    if ($text -match '(?m)([a-z_]+) rejected by server:') { $failureStage = $Matches[1] }
    return [ordered]@{
        failureStage = $failureStage
        frame = $frame
        serverHash = $serverHash
        clientHash = $clientHash
        entityDiff = Get-TextSection -Text $text -Start "entity diffs:" -End "event diffs:"
        eventDiff = Get-TextSection -Text $text -Start "event diffs:" -End "inputs:"
        inputDiff = Get-TextSection -Text $text -Start "inputs:" -End "__never_matches__"
        successEvidenceError = $null
    }
}

function Get-RequiredClientOutputValue {
    param([string]$Text, [string]$Label, [string]$ValuePattern)
    $pattern = "(?m)^" + [regex]::Escape($Label) + "\s*(" + $ValuePattern + ")\s*$"
    $match = [regex]::Match($Text, $pattern)
    if (-not $match.Success) {
        throw "client success output is missing '$Label'"
    }
    return $match.Groups[1].Value
}

function Get-ClientSuccessEvidence {
    param([string]$Stdout, [bool]$ObserverProbe)

    $eventCount = [int](Get-RequiredClientOutputValue `
        -Text $Stdout `
        -Label "final event count:" `
        -ValuePattern "[0-9]+")
    $eventsJson = Get-RequiredClientOutputValue `
        -Text $Stdout `
        -Label "final events json:" `
        -ValuePattern "\[[^\r\n]*\]"
    $eventSummariesJson = Get-RequiredClientOutputValue `
        -Text $Stdout `
        -Label "final event summaries json:" `
        -ValuePattern "\[[^\r\n]*\]"

    try {
        $parsedEvents = $eventsJson | ConvertFrom-Json -ErrorAction Stop
        $finalEvents = @($parsedEvents)
        $parsedEventSummaries = $eventSummariesJson | ConvertFrom-Json -ErrorAction Stop
        $finalEventSummaries = @($parsedEventSummaries)
    } catch {
        throw "client success event JSON could not be parsed: $($_.Exception.Message)"
    }

    if ($eventCount -ne $finalEvents.Count) {
        throw "client success event count mismatch: declared $eventCount, events $($finalEvents.Count)"
    }
    if ($eventCount -ne $finalEventSummaries.Count) {
        throw "client success event summary count mismatch: declared $eventCount, summaries $($finalEventSummaries.Count)"
    }

    $observerRecovery = $null
    if ($ObserverProbe) {
        if (-not [regex]::IsMatch($Stdout, '(?m)^observer recovery:\s*ok\s*$')) {
            throw "client success output is missing 'observer recovery: ok'"
        }
        $observerRecovery = [ordered]@{
            ok = $true
            currentFrame = [int](Get-RequiredClientOutputValue -Text $Stdout -Label "observer current frame:" -ValuePattern "[0-9]+")
            snapshotFrame = [int](Get-RequiredClientOutputValue -Text $Stdout -Label "observer snapshot frame:" -ValuePattern "[0-9]+")
            initialSnapshotFrame = [int](Get-RequiredClientOutputValue -Text $Stdout -Label "observer initial snapshot frame:" -ValuePattern "[0-9]+")
            lastFrame = [int](Get-RequiredClientOutputValue -Text $Stdout -Label "observer last frame:" -ValuePattern "[0-9]+")
            observerLastFrame = [int](Get-RequiredClientOutputValue -Text $Stdout -Label "observer observerFrame.lastFrame:" -ValuePattern "[0-9]+")
            hash = (Get-RequiredClientOutputValue -Text $Stdout -Label "observer hash:" -ValuePattern "[0-9a-fA-F]+").ToLowerInvariant()
        }
    }

    return [ordered]@{
        finalEventCount = $eventCount
        finalEvents = $finalEvents
        finalEventSummaries = $finalEventSummaries
        observerRecovery = $observerRecovery
    }
}

function Read-TextFile {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return "" }
    return Get-Content -LiteralPath $Path -Raw
}

function Get-MybevyTelemetryRecords {
    param([string]$Stdout)
    $records = @()
    foreach ($line in @($Stdout -split "`r?`n")) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        try {
            $record = $line | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw "mybevy telemetry contains invalid JSONL"
        }
        if ($record.schema -ne "mybevy.lockstep.telemetry" -or [int]$record.schemaVersion -ne 1) {
            throw "mybevy telemetry schema mismatch"
        }
        $records += $record
    }
    if ($records.Count -eq 0) { throw "mybevy telemetry output is empty" }
    return @($records)
}

function Get-MybevySuccessEvidence {
    param([string]$Stdout)
    $records = @(Get-MybevyTelemetryRecords -Stdout $Stdout)
    $failures = @($records | Where-Object { $_.event -eq "run_failed" })
    if ($failures.Count -gt 0) {
        $failure = $failures[-1]
        throw "mybevy telemetry failed at $($failure.failureStage): $($failure.errorCode)"
    }
    $frames = @($records | Where-Object { $_.event -eq "frame" })
    if ($frames.Count -eq 0) { throw "mybevy telemetry has no online frame records" }
    $completed = @($records | Where-Object { $_.event -eq "run_completed" })
    if ($completed.Count -ne 1) { throw "mybevy telemetry must contain exactly one run_completed record" }
    $recovery = @($records | Where-Object { $_.event -eq "replay_recovery" })
    if ($recovery.Count -ne 1 -or $recovery[0].replayRecovery.status -ne "verified") {
        throw "mybevy telemetry replay recovery was not verified"
    }
    foreach ($frame in $frames) {
        if (-not $frame.serverConnected) { throw "mybevy online frame is not serverConnected" }
        if (-not $frame.serverHash -or $frame.serverHash.source -ne "my_server_authority") {
            throw "mybevy online frame is missing a MyServer authority hash"
        }
        if (-not $frame.localHash -or $frame.serverHash.hex -ne $frame.localHash.hex -or $frame.mismatch -ne $false) {
            throw "mybevy online frame hash mismatch"
        }
    }

    $commands = @($frames | ForEach-Object { @($_.inputs) } | ForEach-Object { $_.command })
    foreach ($requiredCommand in @("move", "cast_skill", "stop")) {
        if ($commands -notcontains $requiredCommand) { throw "mybevy telemetry is missing $requiredCommand input" }
    }
    $events = @($frames | ForEach-Object { @($_.events.items) })
    $eventKinds = @($events | ForEach-Object { $_.kind })
    if ($eventKinds -notcontains "skill_cast") { throw "mybevy telemetry is missing SkillCast" }
    if ($eventKinds -notcontains "damage_applied" -and $eventKinds -notcontains "buff_applied" -and $eventKinds -notcontains "buff_tick") {
        throw "mybevy telemetry is missing damage or Buff evidence"
    }

    $started = @($records | Where-Object { $_.event -eq "run_started" })
    if ($started.Count -ne 1) { throw "mybevy telemetry must contain exactly one run_started record" }
    $playerId = [string]$completed[0].player
    $startPlayer = @($started[0].entities | Where-Object { $_.ownerCharacterId -eq $playerId })
    $finalPlayer = @($completed[0].entities | Where-Object { $_.ownerCharacterId -eq $playerId })
    if ($startPlayer.Count -ne 1 -or $finalPlayer.Count -ne 1) {
        throw "mybevy telemetry does not identify exactly one controlled player entity"
    }
    if ([long]$startPlayer[0].fixedPositionMilli.x -eq [long]$finalPlayer[0].fixedPositionMilli.x) {
        throw "mybevy telemetry fixed position did not move"
    }

    $finalFrame = [int]$completed[0].frame
    $finalHash = [string]$completed[0].localHash.hex
    return [ordered]@{
        finalFrame = $finalFrame
        finalHash = $finalHash.ToLowerInvariant()
        finalEventCount = $events.Count
        finalEvents = @($events)
        finalEventSummaries = @($events)
        observerRecovery = $null
        telemetry = [ordered]@{
            schema = [string]$completed[0].schema
            schemaVersion = [int]$completed[0].schemaVersion
            serverConnected = $true
            hashMatched = $true
            inputCommands = @($commands)
            eventKinds = @($eventKinds)
            playerEntityId = [int]$finalPlayer[0].entityId
            initialFixedPosition = $startPlayer[0].fixedPositionMilli
            finalFixedPosition = $finalPlayer[0].fixedPositionMilli
            recoveryStatus = [string]$recovery[0].replayRecovery.status
        }
    }
}

function ConvertTo-MybevyTelemetryJson {
    param([AllowNull()][object]$Value)
    return ConvertTo-Json -InputObject @($Value) -Depth 30 -Compress
}

function Get-MybevyDualSuccessEvidence {
    param([string]$Stdout)
    $records = @(Get-MybevyTelemetryRecords -Stdout $Stdout)
    $failures = @($records | Where-Object { $_.event -eq "run_failed" })
    if ($failures.Count -gt 0) {
        $failure = $failures[-1]
        throw "mybevy dual telemetry failed at $($failure.failureStage): $($failure.errorCode)"
    }

    $activeRecords = @($records | Where-Object { $_.clientRole -eq "active_input" })
    $passiveRecords = @($records | Where-Object { $_.clientRole -eq "passive_replay" })
    if ($activeRecords.Count -eq 0 -or $passiveRecords.Count -eq 0) {
        throw "mybevy dual telemetry must identify active_input and passive_replay clients"
    }
    $activePlayer = [string]$activeRecords[0].player
    $passivePlayer = [string]$passiveRecords[0].player
    if ([string]::IsNullOrWhiteSpace($activePlayer) -or [string]::IsNullOrWhiteSpace($passivePlayer) -or $activePlayer -eq $passivePlayer) {
        throw "mybevy dual telemetry players must be present and distinct"
    }
    if (@($activeRecords | Where-Object { $_.player -ne $activePlayer }).Count -gt 0 -or
        @($passiveRecords | Where-Object { $_.player -ne $passivePlayer }).Count -gt 0) {
        throw "mybevy dual telemetry changed player identity within a client stream"
    }

    $activeStarted = @($activeRecords | Where-Object { $_.event -eq "run_started" })
    $passiveStarted = @($passiveRecords | Where-Object { $_.event -eq "run_started" })
    $activeCompleted = @($activeRecords | Where-Object { $_.event -eq "run_completed" })
    $passiveCompleted = @($passiveRecords | Where-Object { $_.event -eq "run_completed" })
    $activeRecovery = @($activeRecords | Where-Object { $_.event -eq "replay_recovery" })
    $passiveRecovery = @($passiveRecords | Where-Object { $_.event -eq "replay_recovery" })
    if ($activeStarted.Count -ne 1 -or $passiveStarted.Count -ne 1 -or
        $activeCompleted.Count -ne 1 -or $passiveCompleted.Count -ne 1 -or
        $activeRecovery.Count -ne 1 -or $passiveRecovery.Count -ne 1) {
        throw "mybevy dual telemetry requires one start, recovery, and completion record per client"
    }
    if ($activeRecovery[0].replayRecovery.status -ne "verified" -or
        $passiveRecovery[0].replayRecovery.status -ne "verified") {
        throw "mybevy dual telemetry replay recovery was not verified for both clients"
    }

    $activeFrames = @($activeRecords | Where-Object { $_.event -eq "frame" } | Sort-Object { [int]$_.frame })
    $passiveFrames = @($passiveRecords | Where-Object { $_.event -eq "frame" } | Sort-Object { [int]$_.frame })
    if ($activeFrames.Count -eq 0 -or $passiveFrames.Count -eq 0) {
        throw "mybevy dual telemetry has no authority frame records for one or both clients"
    }
    $passiveByFrame = @{}
    foreach ($frameRecord in $passiveFrames) {
        $key = [string][int]$frameRecord.frame
        if ($passiveByFrame.ContainsKey($key)) { throw "mybevy passive telemetry contains duplicate frame $key" }
        $passiveByFrame[$key] = $frameRecord
    }

    $comparedFrames = @()
    $allEvents = @()
    $inputSources = @()
    $inputSequences = @()
    foreach ($activeFrame in $activeFrames) {
        $frame = [int]$activeFrame.frame
        $key = [string]$frame
        if (-not $passiveByFrame.ContainsKey($key)) { continue }
        $passiveFrame = $passiveByFrame[$key]
        if (-not $activeFrame.serverConnected -or -not $passiveFrame.serverConnected) {
            throw "dual telemetry first mismatch frame ${frame}: one client was not serverConnected"
        }
        if (-not $activeFrame.serverHash -or -not $passiveFrame.serverHash -or
            -not $activeFrame.localHash -or -not $passiveFrame.localHash) {
            throw "dual telemetry first mismatch frame ${frame}: one client is missing hash telemetry"
        }
        $serverHash = ([string]$activeFrame.serverHash.hex).ToLowerInvariant()
        $activeHash = ([string]$activeFrame.localHash.hex).ToLowerInvariant()
        $passiveServerHash = ([string]$passiveFrame.serverHash.hex).ToLowerInvariant()
        $passiveHash = ([string]$passiveFrame.localHash.hex).ToLowerInvariant()
        if ($activeFrame.serverHash.source -ne "my_server_authority" -or
            $passiveFrame.serverHash.source -ne "my_server_authority" -or
            $serverHash -ne $activeHash -or $serverHash -ne $passiveServerHash -or
            $serverHash -ne $passiveHash -or $activeFrame.mismatch -ne $false -or
            $passiveFrame.mismatch -ne $false) {
            throw "dual telemetry first mismatch frame ${frame}: server/active/passive hash mismatch"
        }

        $activeEntitiesJson = ConvertTo-MybevyTelemetryJson -Value $activeFrame.entities
        $passiveEntitiesJson = ConvertTo-MybevyTelemetryJson -Value $passiveFrame.entities
        if ($activeEntitiesJson -cne $passiveEntitiesJson) {
            throw "dual telemetry first mismatch frame ${frame}: entity fixed state differs"
        }
        $activeEventsJson = ConvertTo-MybevyTelemetryJson -Value $activeFrame.events.items
        $passiveEventsJson = ConvertTo-MybevyTelemetryJson -Value $passiveFrame.events.items
        if ($activeEventsJson -cne $passiveEventsJson) {
            throw "dual telemetry first mismatch frame ${frame}: event sequence differs"
        }
        $activeInputsJson = ConvertTo-MybevyTelemetryJson -Value $activeFrame.inputs
        $passiveInputsJson = ConvertTo-MybevyTelemetryJson -Value $passiveFrame.inputs
        if ($activeInputsJson -cne $passiveInputsJson) {
            throw "dual telemetry first mismatch frame ${frame}: authority input sequence differs"
        }

        $frameInputs = @($activeFrame.inputs)
        foreach ($input in $frameInputs) {
            if ([string]$input.characterId -ne $activePlayer) {
                throw "dual telemetry first mismatch frame ${frame}: input source is not the active player"
            }
            $inputSources += [string]$input.characterId
            $inputSequences += [int]$input.sequence
        }
        $frameEvents = @($activeFrame.events.items)
        $allEvents += $frameEvents
        $comparedFrames += [pscustomobject]@{
            frame = $frame
            serverHash = $serverHash
            activeLocalHash = $activeHash
            passiveLocalHash = $passiveHash
            entityCount = @($activeFrame.entities).Count
            eventKinds = @($frameEvents | ForEach-Object { $_.kind })
            eventSequences = @($frameEvents | ForEach-Object { [int]$_.sequence })
            inputSources = @($frameInputs | ForEach-Object { [string]$_.characterId })
            inputSequences = @($frameInputs | ForEach-Object { [int]$_.sequence })
            inputs = @($frameInputs)
            entities = @($activeFrame.entities | ForEach-Object {
                [pscustomobject]@{
                    entityId = [int]$_.entityId
                    ownerCharacterId = [string]$_.ownerCharacterId
                    fixedPositionMilli = $_.fixedPositionMilli
                    hp = [int]$_.hp
                    alive = [bool]$_.alive
                }
            })
            events = @($frameEvents)
            matched = $true
        }
    }
    if ($comparedFrames.Count -eq 0) { throw "mybevy dual telemetry has no common authority frames" }

    $commands = @($activeFrames | ForEach-Object { @($_.inputs) } | ForEach-Object { $_.command })
    foreach ($requiredCommand in @("move", "cast_skill", "stop")) {
        if ($commands -notcontains $requiredCommand) { throw "mybevy dual telemetry is missing $requiredCommand input" }
    }
    $eventKinds = @($allEvents | ForEach-Object { $_.kind })
    if ($eventKinds -notcontains "skill_cast" -or $eventKinds -notcontains "damage_applied") {
        throw "mybevy dual telemetry is missing SkillCast or DamageApplied evidence"
    }
    if ($inputSequences -notcontains 1 -or $inputSequences -notcontains 2) {
        throw "mybevy dual telemetry is missing active input sequence 1 or 2"
    }
    if (@($inputSources | Select-Object -Unique).Count -ne 1 -or $inputSources[0] -ne $activePlayer) {
        throw "mybevy dual telemetry contains a passive or unknown input source"
    }

    $activeStartEntity = @($activeStarted[0].entities | Where-Object { $_.ownerCharacterId -eq $activePlayer })
    $activeFinalEntity = @($activeCompleted[0].entities | Where-Object { $_.ownerCharacterId -eq $activePlayer })
    $passiveStartEntity = @($passiveStarted[0].entities | Where-Object { $_.ownerCharacterId -eq $passivePlayer })
    $passiveFinalEntity = @($passiveCompleted[0].entities | Where-Object { $_.ownerCharacterId -eq $passivePlayer })
    if ($activeStartEntity.Count -ne 1 -or $activeFinalEntity.Count -ne 1 -or
        $passiveStartEntity.Count -ne 1 -or $passiveFinalEntity.Count -ne 1) {
        throw "mybevy dual telemetry does not identify one controlled entity per client"
    }
    if ([long]$activeStartEntity[0].fixedPositionMilli.x -eq [long]$activeFinalEntity[0].fixedPositionMilli.x) {
        throw "mybevy dual active entity did not move"
    }
    if ((ConvertTo-MybevyTelemetryJson -Value $passiveStartEntity[0]) -cne
        (ConvertTo-MybevyTelemetryJson -Value $passiveFinalEntity[0])) {
        throw "mybevy dual passive controlled entity changed without local input"
    }

    $lastCompared = $comparedFrames[-1]
    return [ordered]@{
        finalFrame = [int]$lastCompared.frame
        finalHash = [string]$lastCompared.serverHash
        finalEventCount = $allEvents.Count
        finalEvents = @($allEvents)
        finalEventSummaries = @($allEvents)
        observerRecovery = $null
        visualSmoke = $null
        telemetry = [ordered]@{
            schema = [string]$activeCompleted[0].schema
            schemaVersion = [int]$activeCompleted[0].schemaVersion
            serverConnected = $true
            hashMatched = $true
            recoveryStatus = "verified"
            dualReconciliation = [ordered]@{
                matched = $true
                firstMismatchFrame = $null
                comparedFrameCount = $comparedFrames.Count
                commonFrameStart = [int]$comparedFrames[0].frame
                commonFrameEnd = [int]$lastCompared.frame
                inputSources = @($inputSources | Select-Object -Unique)
                inputSequences = @($inputSequences | Select-Object -Unique | Sort-Object)
                active = [ordered]@{
                    player = $activePlayer
                    role = "active_input"
                    entityId = [int]$activeFinalEntity[0].entityId
                    initialFixedPosition = $activeStartEntity[0].fixedPositionMilli
                    finalFixedPosition = $activeFinalEntity[0].fixedPositionMilli
                    finalHp = [int]$activeFinalEntity[0].hp
                    finalAlive = [bool]$activeFinalEntity[0].alive
                }
                passive = [ordered]@{
                    player = $passivePlayer
                    role = "passive_replay"
                    entityId = [int]$passiveFinalEntity[0].entityId
                    initialFixedPosition = $passiveStartEntity[0].fixedPositionMilli
                    finalFixedPosition = $passiveFinalEntity[0].fixedPositionMilli
                    finalHp = [int]$passiveFinalEntity[0].hp
                    finalAlive = [bool]$passiveFinalEntity[0].alive
                    localInputAcknowledgements = 0
                }
                frames = @($comparedFrames)
            }
        }
    }
}

function Get-MybevyRecoverySuccessEvidence {
    param([string]$Stdout)
    $records = @(Get-MybevyTelemetryRecords -Stdout $Stdout)
    $failures = @($records | Where-Object { $_.event -eq "run_failed" })
    if ($failures.Count -gt 0) {
        $failure = $failures[-1]
        throw "mybevy recovery telemetry failed at $($failure.failureStage): $($failure.errorCode)"
    }

    $primaryRecords = @($records | Where-Object { $_.clientRole -eq "reconnect_primary" })
    $observerRecords = @($records | Where-Object { $_.clientRole -eq "observer" })
    if ($primaryRecords.Count -eq 0 -or $observerRecords.Count -eq 0) {
        throw "mybevy recovery telemetry must identify reconnect_primary and observer clients"
    }
    $primaryPlayer = [string]$primaryRecords[0].player
    $observerPlayer = [string]$observerRecords[0].player
    if ([string]::IsNullOrWhiteSpace($primaryPlayer) -or [string]::IsNullOrWhiteSpace($observerPlayer) -or $primaryPlayer -eq $observerPlayer) {
        throw "mybevy recovery telemetry players must be present and distinct"
    }

    $disconnected = @($primaryRecords | Where-Object { $_.event -eq "transport_disconnected" })
    $primarySnapshot = @($primaryRecords | Where-Object { $_.event -eq "snapshot_recovered" })
    $observerSnapshot = @($observerRecords | Where-Object { $_.event -eq "snapshot_recovered" })
    $primaryRecovery = @($primaryRecords | Where-Object { $_.event -eq "replay_recovery" })
    $observerRecovery = @($observerRecords | Where-Object { $_.event -eq "replay_recovery" })
    $primaryCompleted = @($primaryRecords | Where-Object { $_.event -eq "run_completed" })
    $observerCompleted = @($observerRecords | Where-Object { $_.event -eq "run_completed" })
    if ($disconnected.Count -ne 1 -or $primarySnapshot.Count -ne 1 -or $observerSnapshot.Count -ne 1 -or
        $primaryRecovery.Count -ne 1 -or $observerRecovery.Count -ne 1 -or
        $primaryCompleted.Count -ne 1 -or $observerCompleted.Count -ne 1) {
        throw "mybevy recovery telemetry requires one disconnect, snapshot recovery, replay recovery, and completion record per applicable client"
    }
    if ($disconnected[0].serverConnected -ne $false) {
        throw "mybevy recovery disconnect record still reports serverConnected"
    }
    foreach ($record in @($primarySnapshot[0], $observerSnapshot[0], $primaryRecovery[0], $observerRecovery[0], $primaryCompleted[0], $observerCompleted[0])) {
        if (-not $record.serverConnected -or -not $record.serverHash -or -not $record.localHash -or
            ([string]$record.serverHash.hex).ToLowerInvariant() -ne ([string]$record.localHash.hex).ToLowerInvariant() -or
            $record.mismatch -ne $false) {
            throw "mybevy recovery snapshot/completion hash evidence is incomplete or mismatched"
        }
    }
    if ($primaryRecovery[0].replayRecovery.status -ne "verified" -or
        $observerRecovery[0].replayRecovery.status -ne "verified") {
        throw "mybevy recovery replay status was not verified for both clients"
    }

    $primaryAcceptance = $primaryRecovery[0].recoveryAcceptance
    $observerAcceptance = $observerRecovery[0].recoveryAcceptance
    if (-not $primaryAcceptance -or -not $observerAcceptance) {
        throw "mybevy recovery acceptance metadata is missing"
    }
    if ([int]$primaryAcceptance.preDisconnectFrame -ne [int]$disconnected[0].frame -or
        [string]$primaryAcceptance.preDisconnectHash -ne ([string]$disconnected[0].localHash.hex).ToLowerInvariant() -or
        [long]$primaryAcceptance.disconnectGeneration -lt 1 -or
        [long]$primaryAcceptance.recoveryGeneration -le [long]$primaryAcceptance.disconnectGeneration) {
        throw "mybevy primary reconnect generation or pre-disconnect evidence is invalid"
    }
    $preDisconnectCommands = @($primaryAcceptance.preDisconnectInputCommands)
    $preDisconnectEvents = @($primaryAcceptance.preDisconnectEventKinds)
    if ([int]$primaryAcceptance.preDisconnectInputFrame -gt [int]$primaryAcceptance.preDisconnectFrame -or
        $preDisconnectCommands -notcontains "move" -or $preDisconnectCommands -notcontains "cast_skill" -or
        $preDisconnectEvents -notcontains "skill_cast" -or $preDisconnectEvents -notcontains "damage_applied") {
        throw "mybevy primary pre-disconnect deterministic input/event evidence is incomplete"
    }
    if ([int]$primaryAcceptance.snapshotFrame -ne [int]$primarySnapshot[0].frame -or
        [int]$observerAcceptance.snapshotFrame -ne [int]$observerSnapshot[0].frame -or
        [string]$primaryAcceptance.snapshotHash -ne ([string]$primarySnapshot[0].localHash.hex).ToLowerInvariant() -or
        [string]$observerAcceptance.snapshotHash -ne ([string]$observerSnapshot[0].localHash.hex).ToLowerInvariant()) {
        throw "mybevy recovery snapshot frame/hash metadata does not match snapshot records"
    }
    foreach ($acceptance in @($primaryAcceptance, $observerAcceptance)) {
        $start = [int]$acceptance.continuityStartFrame
        $end = [int]$acceptance.continuityEndFrame
        $count = [int]$acceptance.continuityFrameCount
        if (-not [bool]$acceptance.contiguousWithoutDuplicateApply -or $start -ne ([int]$acceptance.snapshotFrame + 1) -or
            $end -lt $start -or $count -ne ($end - $start + 1)) {
            throw "mybevy recovery continuity metadata contains a gap or duplicate application"
        }
        if ([int]$acceptance.postReconnectInputApplicationCount -ne 1) {
            throw "mybevy post-reconnect input was not applied exactly once"
        }
    }
    if ([int]$primaryAcceptance.localInputAcknowledgements -lt 2 -or
        [int]$observerAcceptance.localInputAcknowledgements -ne 0 -or
        [bool]$observerAcceptance.hasControlBinding) {
        throw "mybevy observer sent local input or acquired a simulation control binding"
    }
    if ([int]$primaryAcceptance.commonFrameStart -ne [int]$observerAcceptance.commonFrameStart -or
        [int]$primaryAcceptance.commonFrameEnd -ne [int]$observerAcceptance.commonFrameEnd -or
        [int]$primaryAcceptance.commonFrameCount -ne [int]$observerAcceptance.commonFrameCount) {
        throw "mybevy primary and observer common-frame metadata differs"
    }

    $primaryFrames = @($primaryRecords | Where-Object { $_.event -eq "frame" } | Sort-Object { [int]$_.frame })
    $observerFrames = @($observerRecords | Where-Object { $_.event -eq "frame" } | Sort-Object { [int]$_.frame })
    $observerByFrame = @{}
    foreach ($frameRecord in $observerFrames) {
        $key = [string][int]$frameRecord.frame
        if ($observerByFrame.ContainsKey($key)) { throw "mybevy observer telemetry contains duplicate applied frame $key" }
        $observerByFrame[$key] = $frameRecord
    }
    $commonFrames = @()
    $allEvents = @()
    $observerInputSources = @()
    foreach ($primaryFrame in $primaryFrames) {
        $frame = [int]$primaryFrame.frame
        $key = [string]$frame
        if (-not $observerByFrame.ContainsKey($key)) { continue }
        $observerFrame = $observerByFrame[$key]
        if (([string]$primaryFrame.serverHash.hex).ToLowerInvariant() -ne ([string]$primaryFrame.localHash.hex).ToLowerInvariant() -or
            ([string]$primaryFrame.serverHash.hex).ToLowerInvariant() -ne ([string]$observerFrame.serverHash.hex).ToLowerInvariant() -or
            ([string]$primaryFrame.serverHash.hex).ToLowerInvariant() -ne ([string]$observerFrame.localHash.hex).ToLowerInvariant() -or
            (ConvertTo-MybevyTelemetryJson -Value $primaryFrame.entities) -cne (ConvertTo-MybevyTelemetryJson -Value $observerFrame.entities) -or
            (ConvertTo-MybevyTelemetryJson -Value $primaryFrame.inputs) -cne (ConvertTo-MybevyTelemetryJson -Value $observerFrame.inputs) -or
            (ConvertTo-MybevyTelemetryJson -Value $primaryFrame.events.items) -cne (ConvertTo-MybevyTelemetryJson -Value $observerFrame.events.items)) {
            throw "mybevy recovery primary/observer mismatch at frame $frame"
        }
        $frameInputs = @($observerFrame.inputs)
        foreach ($input in $frameInputs) {
            $observerInputSources += [string]$input.characterId
            if ([string]$input.characterId -ne $primaryPlayer) {
                throw "mybevy observer authority input did not originate from the primary player at frame $frame"
            }
        }
        $frameEvents = @($primaryFrame.events.items)
        $allEvents += $frameEvents
        $commonFrames += [pscustomobject]@{
            frame = $frame
            hash = ([string]$primaryFrame.serverHash.hex).ToLowerInvariant()
            inputs = @($primaryFrame.inputs)
            events = @($frameEvents)
            entities = @($primaryFrame.entities)
            matched = $true
        }
    }
    if ($commonFrames.Count -ne [int]$primaryAcceptance.commonFrameCount -or
        [int]$commonFrames[0].frame -ne [int]$primaryAcceptance.commonFrameStart -or
        [int]$commonFrames[-1].frame -ne [int]$primaryAcceptance.commonFrameEnd) {
        throw "mybevy recovery common-frame telemetry does not match acceptance metadata"
    }

    return [ordered]@{
        finalFrame = [int]$primaryCompleted[0].frame
        finalHash = ([string]$primaryCompleted[0].localHash.hex).ToLowerInvariant()
        finalEventCount = $allEvents.Count
        finalEvents = @($allEvents)
        finalEventSummaries = @($allEvents)
        observerRecovery = [ordered]@{
            ok = $true
            player = $observerPlayer
            snapshotFrame = [int]$observerAcceptance.snapshotFrame
            finalFrame = [int]$observerCompleted[0].frame
            hash = ([string]$observerCompleted[0].localHash.hex).ToLowerInvariant()
            recoveryGeneration = [long]$observerAcceptance.recoveryGeneration
            localInputAcknowledgements = [int]$observerAcceptance.localInputAcknowledgements
            hasControlBinding = [bool]$observerAcceptance.hasControlBinding
        }
        visualSmoke = $null
        telemetry = [ordered]@{
            schema = [string]$primaryCompleted[0].schema
            schemaVersion = [int]$primaryCompleted[0].schemaVersion
            serverConnected = $true
            hashMatched = $true
            recoveryStatus = "verified"
            reconnectObserver = [ordered]@{
                matched = $true
                primaryPlayer = $primaryPlayer
                observerPlayer = $observerPlayer
                primary = $primaryAcceptance
                observer = $observerAcceptance
                observerInputSources = @($observerInputSources | Select-Object -Unique)
                frames = @($commonFrames)
            }
        }
    }
}

function Get-MybevyVisualSmokeSuccessEvidence {
    param([string]$ArtifactDirectory)

    $paths = Get-MybevyVisualSmokeArtifactPaths -ArtifactDirectory $ArtifactDirectory
    $online = Read-JsonArtifact -Path $paths.onlineReport -Label "mybevy online visual report"
    $offline = Read-JsonArtifact -Path $paths.offlineReport -Label "mybevy offline fixture report"
    if ($online.schema -ne "mybevy.lockstep.visual-smoke" -or [int]$online.schemaVersion -ne 1) {
        throw "mybevy online visual report schema mismatch"
    }
    if ($offline.schema -ne "mybevy.lockstep.visual-smoke" -or [int]$offline.schemaVersion -ne 1) {
        throw "mybevy offline fixture report schema mismatch"
    }
    if ($online.source -ne "myserver_authority") {
        throw "mybevy online visual report does not identify MyServer authority"
    }
    if ($online.uiMode -ne "robot_sync_scene") {
        throw "mybevy online screenshot was not captured from the gameplay UI"
    }
    if (-not [bool]$online.coreSmokePassed) {
        throw "mybevy online visual smoke did not pass"
    }
    foreach ($field in @("movement", "skillCast", "hitAndDamageNumber", "hudReadable", "hashMatched")) {
        if (-not [bool]$online.coverage.$field) {
            throw "mybevy online visual smoke is missing $field evidence"
        }
    }
    if ([string]::IsNullOrWhiteSpace([string]$online.localHash) -or
        [string]$online.localHash -ne [string]$online.serverHash -or
        [bool]$online.mismatch) {
        throw "mybevy online visual smoke hash evidence does not match"
    }
    if ($offline.source -ne "offline_visual_fixture") {
        throw "mybevy offline visual report source is not isolated from online evidence"
    }
    if ($offline.uiMode -ne "robot_sync_scene") {
        throw "mybevy offline fixture screenshot was not captured from the gameplay UI"
    }
    if (-not [bool]$offline.passed -or $offline.status -ne "passed") {
        throw "mybevy offline Buff/DoT/death fixture did not pass"
    }
    foreach ($field in @("buffApplied", "buffTick", "dotDamageNumber", "deathState")) {
        if (-not [bool]$offline.coverage.$field) {
            throw "mybevy offline visual fixture is missing $field evidence"
        }
    }

    $captureHashes = @{}
    foreach ($capture in @(
        [pscustomobject]@{ label = "online"; expectedPath = $paths.onlineScreenshot; report = $online.screenshot },
        [pscustomobject]@{ label = "offline fixture"; expectedPath = $paths.offlineScreenshot; report = $offline.screenshot }
    )) {
        if (-not $capture.report -or [int]$capture.report.width -lt 1 -or [int]$capture.report.height -lt 1) {
            throw "mybevy $($capture.label) screenshot metadata is missing"
        }
        $expectedPath = [System.IO.Path]::GetFullPath([string]$capture.expectedPath)
        $reportedPath = [System.IO.Path]::GetFullPath([string]$capture.report.path)
        if (-not [string]::Equals($expectedPath, $reportedPath, [StringComparison]::OrdinalIgnoreCase)) {
            throw "mybevy $($capture.label) screenshot path does not match the run-owned artifact path"
        }
        if (-not (Test-Path -LiteralPath $expectedPath -PathType Leaf) -or
            (Get-Item -LiteralPath $expectedPath).Length -lt 1) {
            throw "mybevy $($capture.label) screenshot is missing or empty"
        }
        $captureHashes[$capture.label] = (Get-FileHash -LiteralPath $expectedPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    if ($captureHashes["online"] -eq $captureHashes["offline fixture"]) {
        throw "mybevy online and offline fixture screenshots are byte-identical"
    }

    $eventKinds = @($online.eventKinds)
    return [ordered]@{
        finalFrame = [int]$online.frame
        finalHash = ([string]$online.localHash).ToLowerInvariant()
        finalEventCount = $eventKinds.Count
        finalEvents = @($eventKinds)
        finalEventSummaries = @($eventKinds)
        observerRecovery = $null
        telemetry = $null
        visualSmoke = [ordered]@{
            combinedAcceptanceComplete = $true
            online = [ordered]@{
                source = [string]$online.source
                status = [string]$online.status
                report = [string]$paths.onlineReport
                screenshot = [string]$paths.onlineScreenshot
                screenshotSha256 = [string]$captureHashes["online"]
                coverage = $online.coverage
            }
            offlineFixture = [ordered]@{
                source = [string]$offline.source
                status = [string]$offline.status
                report = [string]$paths.offlineReport
                screenshot = [string]$paths.offlineScreenshot
                screenshotSha256 = [string]$captureHashes["offline fixture"]
                coverage = $offline.coverage
            }
        }
    }
}

function Get-MybevyVisualSmokeDiagnostics {
    param([string]$ArtifactDirectory)
    $diagnostics = [ordered]@{
        errorCode = $null
        failureStage = $null
        frame = $null
        serverHash = $null
        clientHash = $null
        entityDiff = $null
        eventDiff = $null
        inputDiff = $null
        successEvidenceError = $null
    }
    $paths = Get-MybevyVisualSmokeArtifactPaths -ArtifactDirectory $ArtifactDirectory
    if (-not (Test-Path -LiteralPath $paths.onlineReport -PathType Leaf)) {
        return $diagnostics
    }
    try {
        $online = Read-JsonArtifact -Path $paths.onlineReport -Label "mybevy online visual report"
        if ($null -ne $online.frame) { $diagnostics.frame = [int]$online.frame }
        $diagnostics.serverHash = [string]$online.serverHash
        $diagnostics.clientHash = [string]$online.localHash
        $diagnostics.eventDiff = @($online.eventKinds) | ConvertTo-Json -Compress
        if ($online.failure) {
            $diagnostics.failureStage = "visual_smoke"
            $diagnostics.successEvidenceError = [string]$online.failure
        }
    } catch {
        $diagnostics.failureStage = "visual_report_parse"
        $diagnostics.successEvidenceError = $_.Exception.Message
    }
    return $diagnostics
}

function Get-MybevyClientDiagnostics {
    param([string]$Stdout, [string]$Stderr)
    $diagnostics = [ordered]@{
        errorCode = $null
        failureStage = $null
        frame = $null
        serverHash = $null
        clientHash = $null
        entityDiff = $null
        eventDiff = $null
        inputDiff = $null
        successEvidenceError = $null
    }
    try {
        $records = @(Get-MybevyTelemetryRecords -Stdout $Stdout)
        $failure = @($records | Where-Object { $_.event -eq "run_failed" } | Select-Object -Last 1)
        if ($failure.Count -gt 0) {
            $diagnostics.errorCode = [string]$failure[0].errorCode
            $diagnostics.failureStage = [string]$failure[0].failureStage
            if ($null -ne $failure[0].frame) { $diagnostics.frame = [int]$failure[0].frame }
            $diagnostics.entityDiff = @($failure[0].entities) | ConvertTo-Json -Depth 12 -Compress
        }
        $frameRecord = @($records | Where-Object { $_.event -eq "frame" } | Select-Object -Last 1)
        if ($frameRecord.Count -gt 0) {
            if ($null -eq $diagnostics.frame) { $diagnostics.frame = [int]$frameRecord[0].frame }
            if ($frameRecord[0].serverHash) { $diagnostics.serverHash = [string]$frameRecord[0].serverHash.hex }
            if ($frameRecord[0].localHash) { $diagnostics.clientHash = [string]$frameRecord[0].localHash.hex }
            $diagnostics.eventDiff = @($frameRecord[0].events.items) | ConvertTo-Json -Depth 12 -Compress
            $diagnostics.inputDiff = @($frameRecord[0].inputs) | ConvertTo-Json -Depth 12 -Compress
        }
    } catch {
        $diagnostics.failureStage = "telemetry_parse"
        $diagnostics.successEvidenceError = $_.Exception.Message
    }
    return $diagnostics
}

function Get-MybevyDualClientDiagnostics {
    param([string]$Stdout, [string]$Stderr)
    $diagnostics = Get-MybevyClientDiagnostics -Stdout $Stdout -Stderr $Stderr
    try {
        $records = @(Get-MybevyTelemetryRecords -Stdout $Stdout)
        $failure = @($records | Where-Object { $_.event -eq "run_failed" } | Select-Object -Last 1)
        if ($failure.Count -eq 0 -or $null -eq $failure[0].frame) { return $diagnostics }
        $frame = [int]$failure[0].frame
        $activeFrame = @($records | Where-Object {
            $_.event -eq "frame" -and $_.clientRole -eq "active_input" -and [int]$_.frame -eq $frame
        } | Select-Object -Last 1)
        $passiveFrame = @($records | Where-Object {
            $_.event -eq "frame" -and $_.clientRole -eq "passive_replay" -and [int]$_.frame -eq $frame
        } | Select-Object -Last 1)
        $diagnostics.frame = $frame
        if ($activeFrame.Count -eq 1 -and $passiveFrame.Count -eq 1) {
            $diagnostics.serverHash = [string]$activeFrame[0].serverHash.hex
            $diagnostics.clientHash = "active=$($activeFrame[0].localHash.hex);passive=$($passiveFrame[0].localHash.hex)"
            $diagnostics.entityDiff = [ordered]@{
                active = @($activeFrame[0].entities)
                passive = @($passiveFrame[0].entities)
            } | ConvertTo-Json -Depth 30 -Compress
            $diagnostics.eventDiff = [ordered]@{
                active = @($activeFrame[0].events.items)
                passive = @($passiveFrame[0].events.items)
            } | ConvertTo-Json -Depth 30 -Compress
            $diagnostics.inputDiff = [ordered]@{
                active = @($activeFrame[0].inputs)
                passive = @($passiveFrame[0].inputs)
            } | ConvertTo-Json -Depth 30 -Compress
        }
    } catch {
        $diagnostics.failureStage = "telemetry_parse"
        $diagnostics.successEvidenceError = $_.Exception.Message
    }
    return $diagnostics
}

function Invoke-ClientStage {
    param([pscustomobject]$Definition, [string]$Mode, [string]$ArtifactDirectory)
    $stdoutPath = Join-Path $ArtifactDirectory "$($Definition.name).stdout.log"
    $stderrPath = Join-Path $ArtifactDirectory "$($Definition.name).stderr.log"
    $arguments = New-ClientArguments -Stage $Definition -Mode $Mode
    if ($Mode -eq "execute" -and $Client -eq "mybevy" -and $Definition.visualSmoke) {
        $null = Set-MybevyVisualSmokeEnvironment -Definition $Definition -ArtifactDirectory $ArtifactDirectory
    }
    $workingDirectory = if ($Client -eq "mybevy" -and $Definition.visualSmoke) {
        Split-Path -Parent $MybevyManifestPath
    } else {
        $ProjectRoot
    }
    $startedAt = Get-NowIso
    $exitCode = Invoke-NativeCaptured `
        -FilePath "cargo" `
        -Arguments $arguments `
        -StdoutPath $stdoutPath `
        -StderrPath $stderrPath `
        -WorkingDirectory $workingDirectory
    $endedAt = Get-NowIso
    $stdout = Read-TextFile -Path $stdoutPath
    $stderr = Read-TextFile -Path $stderrPath
    $diagnostics = if ($Client -eq "mybevy" -and $Definition.visualSmoke) {
        Get-MybevyVisualSmokeDiagnostics -ArtifactDirectory $ArtifactDirectory
    } elseif ($Client -eq "mybevy" -and $Definition.reconnectObserver) {
        Get-MybevyClientDiagnostics -Stdout $stdout -Stderr $stderr
    } elseif ($Client -eq "mybevy" -and $Definition.dualClient) {
        Get-MybevyDualClientDiagnostics -Stdout $stdout -Stderr $stderr
    } elseif ($Client -eq "mybevy") {
        Get-MybevyClientDiagnostics -Stdout $stdout -Stderr $stderr
    } else {
        Get-ClientDiagnostics -Stdout $stdout -Stderr $stderr
    }
    $finalFrame = $null
    $finalHash = $null
    if ($Client -ne "mybevy") {
        if ($stdout -match '(?m)^final frame:\s*([0-9]+)') { $finalFrame = [int]$Matches[1] }
        if ($stdout -match '(?m)^final hash:\s*([0-9a-fA-F]+)') { $finalHash = $Matches[1].ToLowerInvariant() }
    }
    $successEvidence = [ordered]@{
        finalEventCount = $null
        finalEvents = @()
        finalEventSummaries = @()
        observerRecovery = $null
        telemetry = $null
        visualSmoke = $null
    }
    $stageExitCode = $exitCode
    if ($Mode -eq "execute" -and $exitCode -eq 0) {
        try {
            $successEvidence = if ($Client -eq "mybevy" -and $Definition.visualSmoke) {
                Get-MybevyVisualSmokeSuccessEvidence -ArtifactDirectory $ArtifactDirectory
            } elseif ($Client -eq "mybevy" -and $Definition.reconnectObserver) {
                Get-MybevyRecoverySuccessEvidence -Stdout $stdout
            } elseif ($Client -eq "mybevy" -and $Definition.dualClient) {
                Get-MybevyDualSuccessEvidence -Stdout $stdout
            } elseif ($Client -eq "mybevy") {
                Get-MybevySuccessEvidence -Stdout $stdout
            } else {
                Get-ClientSuccessEvidence `
                    -Stdout $stdout `
                    -ObserverProbe ([bool]$Definition.observerProbe)
            }
            if ($Client -eq "mybevy") {
                $finalFrame = $successEvidence.finalFrame
                $finalHash = $successEvidence.finalHash
            }
        } catch {
            $stageExitCode = 1
            $diagnostics.failureStage = "success_evidence"
            $diagnostics.successEvidenceError = $_.Exception.Message
        }
    }
    if ($Definition.visualSmoke -and $exitCode -ne 0 -and -not $diagnostics.failureStage) {
        $diagnostics.failureStage = "visual_smoke_process"
        $diagnostics.successEvidenceError = "mybevy GUI process exited with code $exitCode; see captured logs"
    }
    $result = [ordered]@{
        name = $Definition.name
        scenario = $Definition.scenario
        roomId = $Definition.roomId
        observerProbe = [bool]$Definition.observerProbe
        dualClient = [bool]$Definition.dualClient
        reconnectObserver = [bool]$Definition.reconnectObserver
        status = if ($stageExitCode -eq 0) { "passed" } else { "failed" }
        exitCode = $stageExitCode
        processExitCode = $exitCode
        startedAt = $startedAt
        endedAt = $endedAt
        finalFrame = $finalFrame
        finalHash = $finalHash
        finalEventCount = $successEvidence.finalEventCount
        finalEvents = @($successEvidence.finalEvents)
        finalEventSummaries = @($successEvidence.finalEventSummaries)
        observerRecovery = $successEvidence.observerRecovery
        telemetry = $successEvidence.telemetry
        visualSmoke = $successEvidence.visualSmoke
        diagnostics = $diagnostics
        stdout = $stdoutPath
        stderr = $stderrPath
    }
    if ($successEvidence.observerRecovery) {
        $result["observerHash"] = $successEvidence.observerRecovery.hash
    }
    return $result
}

function Invoke-SelfTests {
    $testRunId = "20260710-120000-a1b2c3d4"
    $ephemeralSecretA = New-EphemeralTicketSecret
    $ephemeralSecretB = New-EphemeralTicketSecret
    if ($ephemeralSecretA.Length -lt 40 -or
        $ephemeralSecretA -notmatch '^[A-Za-z0-9_-]+$' -or
        $ephemeralSecretA -eq $ephemeralSecretB) {
        throw "self-test: ephemeral ticket secret generation failed"
    }
    $invalidEnvRejected = $false
    try {
        Assert-EnvironmentVariableName -Name "INVALID-NAME" -ParameterName "self-test"
    } catch {
        $invalidEnvRejected = $true
    }
    if (-not $invalidEnvRejected) { throw "self-test: invalid environment variable name was accepted" }
    $savedTicketEnvVar = $script:TicketEnvVar
    $reservedAliasRejected = $false
    try {
        $script:TicketEnvVar = "TICKET_SECRET"
        Assert-RunOptions -Mode "self-test" -Checks @("move")
    } catch {
        $reservedAliasRejected = $true
    } finally {
        $script:TicketEnvVar = $savedTicketEnvVar
    }
    if (-not $reservedAliasRejected) { throw "self-test: reserved runtime environment alias was accepted" }
    $definitions = New-StageDefinitions -Checks @("move", "melee", "observer") -CurrentRunId $testRunId
    if ($definitions.Count -ne 3) { throw "self-test: expected three stage definitions" }
    $executeArgs = New-ClientArguments -Stage $definitions[2] -Mode "execute"
    $command = Format-Command -Executable "cargo" -Arguments $executeArgs
    if ($command -notmatch '--ticket-env MYSERVER_LOCKSTEP_TICKET') { throw "self-test: primary ticket env argument missing" }
    if ($command -notmatch '--observer-ticket-env MYSERVER_LOCKSTEP_OBSERVER_TICKET') { throw "self-test: observer ticket env argument missing" }
    if ($command -match 'secret-ticket-value') { throw "self-test: command leaked ticket value" }
    $dryArgs = New-ClientArguments -Stage $definitions[0] -Mode "dry-run"
    if ($dryArgs -notcontains "--dry-run" -or $dryArgs -contains "--ticket-env") { throw "self-test: dry-run command is not network-free" }
    $savedClient = $script:Client
    $savedMybevyManifestPath = $script:MybevyManifestPath
    $savedRunIdForClient = $script:RunId
    try {
        $script:Client = "mybevy"
        $script:MybevyManifestPath = "C:\client\project\Cargo.toml"
        $script:RunId = $testRunId
        $mybevyDefinition = (New-StageDefinitions -Checks @("single-client") -CurrentRunId $testRunId)[0]
        $mybevyArgs = New-ClientArguments -Stage $mybevyDefinition -Mode "execute"
        $mybevyCommand = Format-Command -Executable "cargo" -Arguments $mybevyArgs
        if ($mybevyCommand -notmatch '--bin lockstep-sim-headless' -or
            $mybevyCommand -notmatch '--scenario online-single-client' -or
            $mybevyCommand -notmatch '--ticket-env MYSERVER_LOCKSTEP_TICKET') {
            throw "self-test: mybevy command assembly is incomplete"
        }
        if ($mybevyCommand -match 'secret-ticket-value') { throw "self-test: mybevy command leaked ticket value" }
        $mybevyDryArgs = New-ClientArguments -Stage $mybevyDefinition -Mode "dry-run"
        if ($mybevyDryArgs -contains "--ticket-env" -or $mybevyDryArgs -contains "--endpoint" -or $mybevyDryArgs -notcontains "offline-fixture") {
            throw "self-test: mybevy dry-run command is not network-free"
        }
        $dualDefinition = (New-StageDefinitions -Checks @("dual-client") -CurrentRunId $testRunId)[0]
        $dualArgs = New-ClientArguments -Stage $dualDefinition -Mode "execute"
        $dualCommand = Format-Command -Executable "cargo" -Arguments $dualArgs
        if ($dualCommand -notmatch '--scenario online-dual-client' -or
            $dualCommand -notmatch '--ticket-env MYSERVER_LOCKSTEP_TICKET' -or
            $dualCommand -notmatch '--observer-ticket-env MYSERVER_LOCKSTEP_OBSERVER_TICKET') {
            throw "self-test: mybevy dual-client command assembly is incomplete"
        }
        if ($dualCommand -match 'secret-ticket-value') { throw "self-test: mybevy dual-client command leaked ticket value" }
        $dualDryArgs = New-ClientArguments -Stage $dualDefinition -Mode "dry-run"
        if ($dualDryArgs -contains "--ticket-env" -or $dualDryArgs -contains "--observer-ticket-env" -or
            $dualDryArgs -contains "--endpoint" -or $dualDryArgs -notcontains "offline-fixture") {
            throw "self-test: mybevy dual-client dry-run command is not network-free"
        }
        $recoveryDefinition = (New-StageDefinitions -Checks @("reconnect-observer") -CurrentRunId $testRunId)[0]
        $recoveryArgs = New-ClientArguments -Stage $recoveryDefinition -Mode "execute"
        $recoveryCommand = Format-Command -Executable "cargo" -Arguments $recoveryArgs
        if ($recoveryCommand -notmatch '--scenario online-reconnect-observer' -or
            $recoveryCommand -notmatch '--ticket-env MYSERVER_LOCKSTEP_TICKET' -or
            $recoveryCommand -notmatch '--observer-ticket-env MYSERVER_LOCKSTEP_OBSERVER_TICKET') {
            throw "self-test: mybevy reconnect-observer command assembly is incomplete"
        }
        if ($recoveryCommand -match 'secret-ticket-value') { throw "self-test: mybevy reconnect-observer command leaked ticket value" }
        $recoveryDryArgs = New-ClientArguments -Stage $recoveryDefinition -Mode "dry-run"
        if ($recoveryDryArgs -contains "--ticket-env" -or $recoveryDryArgs -contains "--observer-ticket-env" -or
            $recoveryDryArgs -contains "--endpoint" -or $recoveryDryArgs -notcontains "offline-fixture") {
            throw "self-test: mybevy reconnect-observer dry-run command is not network-free"
        }
        $visualDefinition = (New-StageDefinitions -Checks @("visual-smoke") -CurrentRunId $testRunId)[0]
        $visualArgs = New-ClientArguments -Stage $visualDefinition -Mode "execute"
        $visualCommand = Format-Command -Executable "cargo" -Arguments $visualArgs
        if ($visualCommand -notmatch '--bin project' -or
            $visualCommand -notmatch '--window-profile desktop' -or
            $visualCommand -match '--ticket-env|secret-ticket-value') {
            throw "self-test: mybevy visual smoke command assembly is unsafe or incomplete"
        }
        $visualDryArgs = New-ClientArguments -Stage $visualDefinition -Mode "dry-run"
        if ($visualDryArgs -contains "--ticket-env" -or $visualDryArgs -contains "--endpoint" -or $visualDryArgs -notcontains "offline-fixture") {
            throw "self-test: mybevy visual smoke dry-run command is not network-free"
        }
        $savedVisualEnvironment = @{}
        foreach ($name in $MybevyVisualSmokeEnvironmentNames) {
            $savedVisualEnvironment[$name] = Get-EnvironmentValue -Name $name
        }
        try {
            $visualPaths = Set-MybevyVisualSmokeEnvironment `
                -Definition $visualDefinition `
                -ArtifactDirectory "C:\temp\mybevy visual smoke"
            if ((Get-EnvironmentValue -Name "LOCKSTEP_SIM_MYSERVER_TICKET_ENV") -ne $TicketEnvVar -or
                (Get-EnvironmentValue -Name "LOCKSTEP_SIM_MYSERVER_ROOM") -ne $visualDefinition.roomId -or
                (Get-EnvironmentValue -Name "MYSERVER_GAME_HOST") -ne "127.0.0.1" -or
                (Get-EnvironmentValue -Name "MYSERVER_TCP_FALLBACK_PORT") -ne "7000" -or
                $visualPaths.onlineScreenshot -notmatch 'mybevy-online\.png$' -or
                $visualPaths.offlineReport -notmatch 'offline-fixture-report\.json$') {
                throw "self-test: mybevy visual smoke environment assembly is incomplete"
            }
            $renderedVisualEnvironment = @($MybevyVisualSmokeEnvironmentNames | ForEach-Object {
                "$_=$(Get-EnvironmentValue -Name $_)"
            }) -join "`n"
            if ($renderedVisualEnvironment -match 'secret-ticket-value') {
                throw "self-test: mybevy visual smoke environment leaked a ticket value"
            }
        } finally {
            foreach ($name in $MybevyVisualSmokeEnvironmentNames) {
                Set-ProcessEnvironmentValue -Name $name -Value $savedVisualEnvironment[$name]
            }
        }
    } finally {
        $script:Client = $savedClient
        $script:MybevyManifestPath = $savedMybevyManifestPath
        $script:RunId = $savedRunIdForClient
    }
    $mybevyStdout = @(
        '{"schema":"mybevy.lockstep.telemetry","schemaVersion":1,"event":"run_started","player":"chr-a","serverConnected":true,"frame":0,"serverHash":{"hex":"aaaa","source":"my_server_authority"},"localHash":{"hex":"aaaa"},"mismatch":false,"inputs":[],"entities":[{"entityId":1000,"ownerCharacterId":"chr-a","fixedPositionMilli":{"x":0,"y":0}}],"events":{"items":[]},"replayRecovery":{"status":"checkpoint_captured"}}',
        '{"schema":"mybevy.lockstep.telemetry","schemaVersion":1,"event":"frame","player":"chr-a","serverConnected":true,"frame":2,"serverHash":{"hex":"bbbb","source":"my_server_authority"},"localHash":{"hex":"bbbb"},"mismatch":false,"inputs":[{"command":"move"},{"command":"cast_skill"},{"command":"stop"}],"entities":[{"entityId":1000,"ownerCharacterId":"chr-a","fixedPositionMilli":{"x":300,"y":0}}],"events":{"items":[{"kind":"skill_cast"},{"kind":"damage_applied"}]},"replayRecovery":{"status":"pending"}}',
        '{"schema":"mybevy.lockstep.telemetry","schemaVersion":1,"event":"replay_recovery","player":"chr-a","serverConnected":true,"frame":2,"serverHash":{"hex":"bbbb","source":"my_server_authority"},"localHash":{"hex":"bbbb"},"mismatch":false,"inputs":[],"entities":[{"entityId":1000,"ownerCharacterId":"chr-a","fixedPositionMilli":{"x":300,"y":0}}],"events":{"items":[]},"replayRecovery":{"status":"verified"}}',
        '{"schema":"mybevy.lockstep.telemetry","schemaVersion":1,"event":"run_completed","player":"chr-a","serverConnected":true,"frame":2,"serverHash":{"hex":"bbbb","source":"my_server_authority"},"localHash":{"hex":"bbbb"},"mismatch":false,"inputs":[],"entities":[{"entityId":1000,"ownerCharacterId":"chr-a","fixedPositionMilli":{"x":300,"y":0}}],"events":{"items":[]},"replayRecovery":{"status":"verified"}}'
    ) -join "`n"
    $mybevyEvidence = Get-MybevySuccessEvidence -Stdout $mybevyStdout
    if ($mybevyEvidence.finalFrame -ne 2 -or
        $mybevyEvidence.finalHash -ne "bbbb" -or
        $mybevyEvidence.finalEventCount -ne 2 -or
        -not $mybevyEvidence.telemetry.serverConnected -or
        -not $mybevyEvidence.telemetry.hashMatched) {
        throw "self-test: mybevy telemetry evidence parser failed"
    }
    $dualEntitiesInitial = '[{"entityId":1000,"ownerCharacterId":"chr-a","fixedPositionMilli":{"x":0,"y":0},"hp":100,"alive":true},{"entityId":1001,"ownerCharacterId":"chr-b","fixedPositionMilli":{"x":1000,"y":0},"hp":100,"alive":true}]'
    $dualEntitiesFinal = '[{"entityId":1000,"ownerCharacterId":"chr-a","fixedPositionMilli":{"x":300,"y":0},"hp":100,"alive":true},{"entityId":1001,"ownerCharacterId":"chr-b","fixedPositionMilli":{"x":1000,"y":0},"hp":100,"alive":true}]'
    $dualInputs = '[{"characterId":"chr-a","entityId":1000,"sequence":1,"command":"move"},{"characterId":"chr-a","entityId":1000,"sequence":1,"command":"cast_skill"},{"characterId":"chr-a","entityId":1000,"sequence":2,"command":"stop"}]'
    $dualEvents = '[{"kind":"skill_cast","sequence":1},{"kind":"damage_applied","sequence":2}]'
    $dualStdout = @(
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"run_started`",`"clientRole`":`"active_input`",`"player`":`"chr-a`",`"serverConnected`":true,`"frame`":0,`"serverHash`":{`"hex`":`"aaaa`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"aaaa`"},`"mismatch`":false,`"inputs`":[],`"entities`":$dualEntitiesInitial,`"events`":{`"items`":[]},`"replayRecovery`":{`"status`":`"checkpoint_captured`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"frame`",`"clientRole`":`"active_input`",`"player`":`"chr-a`",`"serverConnected`":true,`"frame`":2,`"serverHash`":{`"hex`":`"bbbb`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"bbbb`"},`"mismatch`":false,`"inputs`":$dualInputs,`"entities`":$dualEntitiesFinal,`"events`":{`"items`":$dualEvents},`"replayRecovery`":{`"status`":`"pending`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"replay_recovery`",`"clientRole`":`"active_input`",`"player`":`"chr-a`",`"serverConnected`":true,`"frame`":2,`"serverHash`":{`"hex`":`"bbbb`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"bbbb`"},`"mismatch`":false,`"inputs`":[],`"entities`":$dualEntitiesFinal,`"events`":{`"items`":[]},`"replayRecovery`":{`"status`":`"verified`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"run_completed`",`"clientRole`":`"active_input`",`"player`":`"chr-a`",`"serverConnected`":true,`"frame`":2,`"serverHash`":{`"hex`":`"bbbb`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"bbbb`"},`"mismatch`":false,`"inputs`":[],`"entities`":$dualEntitiesFinal,`"events`":{`"items`":[]},`"replayRecovery`":{`"status`":`"verified`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"run_started`",`"clientRole`":`"passive_replay`",`"player`":`"chr-b`",`"serverConnected`":true,`"frame`":0,`"serverHash`":{`"hex`":`"aaaa`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"aaaa`"},`"mismatch`":false,`"inputs`":[],`"entities`":$dualEntitiesInitial,`"events`":{`"items`":[]},`"replayRecovery`":{`"status`":`"checkpoint_captured`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"frame`",`"clientRole`":`"passive_replay`",`"player`":`"chr-b`",`"serverConnected`":true,`"frame`":2,`"serverHash`":{`"hex`":`"bbbb`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"bbbb`"},`"mismatch`":false,`"inputs`":$dualInputs,`"entities`":$dualEntitiesFinal,`"events`":{`"items`":$dualEvents},`"replayRecovery`":{`"status`":`"pending`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"replay_recovery`",`"clientRole`":`"passive_replay`",`"player`":`"chr-b`",`"serverConnected`":true,`"frame`":2,`"serverHash`":{`"hex`":`"bbbb`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"bbbb`"},`"mismatch`":false,`"inputs`":[],`"entities`":$dualEntitiesFinal,`"events`":{`"items`":[]},`"replayRecovery`":{`"status`":`"verified`"}}",
        "{`"schema`":`"mybevy.lockstep.telemetry`",`"schemaVersion`":1,`"event`":`"run_completed`",`"clientRole`":`"passive_replay`",`"player`":`"chr-b`",`"serverConnected`":true,`"frame`":2,`"serverHash`":{`"hex`":`"bbbb`",`"source`":`"my_server_authority`"},`"localHash`":{`"hex`":`"bbbb`"},`"mismatch`":false,`"inputs`":[],`"entities`":$dualEntitiesFinal,`"events`":{`"items`":[]},`"replayRecovery`":{`"status`":`"verified`"}}"
    ) -join "`n"
    $dualEvidence = Get-MybevyDualSuccessEvidence -Stdout $dualStdout
    if ($dualEvidence.finalFrame -ne 2 -or $dualEvidence.finalHash -ne "bbbb" -or
        -not $dualEvidence.telemetry.dualReconciliation.matched -or
        $dualEvidence.telemetry.dualReconciliation.comparedFrameCount -ne 1 -or
        $dualEvidence.telemetry.dualReconciliation.active.player -ne "chr-a" -or
        $dualEvidence.telemetry.dualReconciliation.passive.player -ne "chr-b" -or
        @($dualEvidence.telemetry.dualReconciliation.inputSequences).Count -ne 2) {
        throw "self-test: mybevy dual telemetry reconciliation parser failed"
    }
    $primaryAcceptance = [ordered]@{
        preDisconnectFrame = 5; preDisconnectHash = "aaaa"; disconnectGeneration = 2
        preDisconnectInputFrame = 2; preDisconnectInputCommands = @("move", "cast_skill")
        preDisconnectEventKinds = @("skill_cast", "damage_applied")
        snapshotFrame = 5; snapshotHash = "aaaa"; responseCurrentFrame = 5; responseWaitingFrame = 6
        responseRecentInputFrames = @(5); responseWaitingInputFrames = @(6); recoveryGeneration = 3
        continuityStartFrame = 6; continuityEndFrame = 7; continuityFrameCount = 2
        contiguousWithoutDuplicateApply = $true; ignoredDuplicateOrOldFrames = 0
        postReconnectInputFrame = 6; postReconnectInputApplicationCount = 1
        localInputAcknowledgements = 2; hasControlBinding = $true
        commonFrameStart = 6; commonFrameEnd = 7; commonFrameCount = 2
    }
    $observerAcceptance = [ordered]@{
        preDisconnectFrame = $null; preDisconnectHash = $null; disconnectGeneration = $null
        preDisconnectInputFrame = $null; preDisconnectInputCommands = @(); preDisconnectEventKinds = @()
        snapshotFrame = 5; snapshotHash = "aaaa"; responseCurrentFrame = 5; responseWaitingFrame = 6
        responseRecentInputFrames = @(5); responseWaitingInputFrames = @(6); recoveryGeneration = 1
        continuityStartFrame = 6; continuityEndFrame = 7; continuityFrameCount = 2
        contiguousWithoutDuplicateApply = $true; ignoredDuplicateOrOldFrames = 0
        postReconnectInputFrame = 6; postReconnectInputApplicationCount = 1
        localInputAcknowledgements = 0; hasControlBinding = $false
        commonFrameStart = 6; commonFrameEnd = 7; commonFrameCount = 2
    }
    $recoveryEntities = @([ordered]@{
        entityId = 1000; ownerCharacterId = "chr-primary"; fixedPositionMilli = [ordered]@{ x = 300; y = 0 }
        hp = 100; alive = $true
    })
    $recoveryInput = @([ordered]@{
        characterId = "chr-primary"; entityId = 1000; sequence = 2; command = "stop"
    })
    $newRecoveryRecord = {
        param($Role, $Player, $Event, $Frame, $Hash, $Connected, $Inputs, $Acceptance, $RecoveryStatus)
        return [ordered]@{
            schema = "mybevy.lockstep.telemetry"; schemaVersion = 1; event = $Event
            clientRole = $Role; player = $Player; serverConnected = $Connected; frame = $Frame
            serverHash = [ordered]@{ hex = $Hash; source = "my_server_authority" }
            localHash = [ordered]@{ hex = $Hash; source = "local_replay" }
            mismatch = $false; inputs = @($Inputs); entities = @($recoveryEntities)
            events = [ordered]@{ items = @() }; replayRecovery = [ordered]@{ status = $RecoveryStatus }
            recoveryAcceptance = $Acceptance
        }
    }
    $recoveryRecords = @(
        (& $newRecoveryRecord "reconnect_primary" "chr-primary" "transport_disconnected" 5 "aaaa" $false @() $null "pending"),
        (& $newRecoveryRecord "reconnect_primary" "chr-primary" "snapshot_recovered" 5 "aaaa" $true @() $primaryAcceptance "checkpoint_captured"),
        (& $newRecoveryRecord "reconnect_primary" "chr-primary" "frame" 6 "bbbb" $true $recoveryInput $null "pending"),
        (& $newRecoveryRecord "reconnect_primary" "chr-primary" "frame" 7 "cccc" $true @() $null "pending"),
        (& $newRecoveryRecord "reconnect_primary" "chr-primary" "replay_recovery" 7 "cccc" $true @() $primaryAcceptance "verified"),
        (& $newRecoveryRecord "reconnect_primary" "chr-primary" "run_completed" 7 "cccc" $true @() $primaryAcceptance "verified"),
        (& $newRecoveryRecord "observer" "chr-observer" "snapshot_recovered" 5 "aaaa" $true @() $observerAcceptance "checkpoint_captured"),
        (& $newRecoveryRecord "observer" "chr-observer" "frame" 6 "bbbb" $true $recoveryInput $observerAcceptance "pending"),
        (& $newRecoveryRecord "observer" "chr-observer" "frame" 7 "cccc" $true @() $observerAcceptance "pending"),
        (& $newRecoveryRecord "observer" "chr-observer" "replay_recovery" 7 "cccc" $true @() $observerAcceptance "verified"),
        (& $newRecoveryRecord "observer" "chr-observer" "run_completed" 7 "cccc" $true @() $observerAcceptance "verified")
    )
    $recoveryStdout = @($recoveryRecords | ForEach-Object { $_ | ConvertTo-Json -Depth 30 -Compress }) -join "`n"
    $recoveryEvidence = Get-MybevyRecoverySuccessEvidence -Stdout $recoveryStdout
    if ($recoveryEvidence.finalFrame -ne 7 -or $recoveryEvidence.finalHash -ne "cccc" -or
        -not $recoveryEvidence.telemetry.reconnectObserver.matched -or
        $recoveryEvidence.telemetry.reconnectObserver.primary.recoveryGeneration -ne 3 -or
        $recoveryEvidence.observerRecovery.localInputAcknowledgements -ne 0 -or
        $recoveryEvidence.observerRecovery.hasControlBinding) {
        throw "self-test: mybevy reconnect-observer telemetry parser failed"
    }
    $observerInputRejected = $false
    $observerInputError = $null
    try {
        $decodedBadRecords = ConvertTo-Json -InputObject @($recoveryRecords) -Depth 30 | ConvertFrom-Json
        $badRecords = @($decodedBadRecords | ForEach-Object { $_ })
        foreach ($record in $badRecords) {
            if ($record.clientRole -eq "observer" -and $record.recoveryAcceptance) {
                $record.recoveryAcceptance.localInputAcknowledgements = 1
            }
        }
        Get-MybevyRecoverySuccessEvidence -Stdout (@($badRecords | ForEach-Object { $_ | ConvertTo-Json -Depth 30 -Compress }) -join "`n") | Out-Null
    } catch {
        $observerInputError = $_.Exception.Message
        $observerInputRejected = $_.Exception.Message -match 'observer sent local input'
    }
    if (-not $observerInputRejected) { throw "self-test: mybevy recovery parser accepted observer local input (parserResult=$observerInputError)" }
    $mismatchDiagnostics = Get-MybevyClientDiagnostics -Stdout ('{"schema":"mybevy.lockstep.telemetry","schemaVersion":1,"event":"run_failed","errorCode":"HEADLESS_SNAPSHOT_CONFIG_HASH_MISMATCH","failureStage":"snapshot_config_validation","frame":5,"entities":[]}') -Stderr ""
    if ($mismatchDiagnostics.errorCode -ne "HEADLESS_SNAPSHOT_CONFIG_HASH_MISMATCH" -or
        $mismatchDiagnostics.failureStage -ne "snapshot_config_validation") {
        throw "self-test: mybevy recovery mismatch diagnostics lost stable code or stage"
    }
    $visualEvidenceDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "myserver visual evidence $([Guid]::NewGuid().ToString('N'))"
    $visualEvidencePaths = Get-MybevyVisualSmokeArtifactPaths -ArtifactDirectory $visualEvidenceDirectory
    try {
        New-Item -ItemType Directory -Path $visualEvidenceDirectory | Out-Null
        Set-Content -LiteralPath $visualEvidencePaths.onlineScreenshot -Value "online-image" -Encoding ASCII
        Set-Content -LiteralPath $visualEvidencePaths.offlineScreenshot -Value "offline-image" -Encoding ASCII
        [ordered]@{
            schema = "mybevy.lockstep.visual-smoke"
            schemaVersion = 1
            source = "myserver_authority"
            uiMode = "robot_sync_scene"
            status = "captured_with_fixture_gaps"
            coreSmokePassed = $true
            frame = 4
            localHash = "aabb"
            serverHash = "aabb"
            mismatch = $false
            eventKinds = @("skill_cast", "damage_applied")
            coverage = [ordered]@{ movement = $true; skillCast = $true; hitAndDamageNumber = $true; hudReadable = $true; hashMatched = $true }
            screenshot = [ordered]@{ path = $visualEvidencePaths.onlineScreenshot; width = 1280; height = 720 }
        } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $visualEvidencePaths.onlineReport -Encoding UTF8
        [ordered]@{
            schema = "mybevy.lockstep.visual-smoke"
            schemaVersion = 1
            source = "offline_visual_fixture"
            uiMode = "robot_sync_scene"
            status = "passed"
            passed = $true
            coverage = [ordered]@{ buffApplied = $true; buffTick = $true; dotDamageNumber = $true; deathState = $true }
            screenshot = [ordered]@{ path = $visualEvidencePaths.offlineScreenshot; width = 1280; height = 720 }
        } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $visualEvidencePaths.offlineReport -Encoding UTF8
        $visualEvidence = Get-MybevyVisualSmokeSuccessEvidence -ArtifactDirectory $visualEvidenceDirectory
        if (-not $visualEvidence.visualSmoke.combinedAcceptanceComplete -or
            $visualEvidence.finalFrame -ne 4 -or
            $visualEvidence.finalHash -ne "aabb" -or
            $visualEvidence.visualSmoke.online.source -ne "myserver_authority" -or
            $visualEvidence.visualSmoke.offlineFixture.source -ne "offline_visual_fixture") {
            throw "self-test: mybevy visual smoke evidence parser failed"
        }
    } finally {
        foreach ($path in @(
            $visualEvidencePaths.onlineScreenshot,
            $visualEvidencePaths.onlineReport,
            $visualEvidencePaths.offlineScreenshot,
            $visualEvidencePaths.offlineReport
        )) {
            Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $visualEvidenceDirectory -Force -ErrorAction SilentlyContinue
    }
    $diagnostics = Get-ClientDiagnostics -Stdout "" -Stderr "online mismatch: first mismatch frame 7`nserver_hash: aabb`nclient_hash: ccdd`nentity diffs:`n  entity 1`nevent diffs:`n  events differ`ninputs:`n  move"
    if ($diagnostics.frame -ne 7 -or $diagnostics.serverHash -ne "aabb" -or $diagnostics.clientHash -ne "ccdd") { throw "self-test: diagnostic parser failed" }
    $cleanupDiagnostics = Get-ClientDiagnostics -Stdout "" -Stderr "online cleanup failed: room_end rejected by server: END_FAILED"
    if ($cleanupDiagnostics.failureStage -ne "room_end") { throw "self-test: cleanup failure stage parser failed" }
    $successStdout = @(
        "final event count: 2",
        'final events json: [{"type":"skill_cast","frame":1,"value":1},{"type":"damage_applied","frame":1,"value":14}]',
        'final event summaries json: [{"kind":"skillCast","frame":1,"amount":1},{"kind":"damage","frame":1,"amount":14}]',
        "observer recovery: ok",
        "observer current frame: 5",
        "observer snapshot frame: 5",
        "observer initial snapshot frame: 0",
        "observer last frame: 5",
        "observer observerFrame.lastFrame: 5",
        "observer hash: AABBCCDDEEFF0011"
    ) -join "`n"
    $successEvidence = Get-ClientSuccessEvidence -Stdout $successStdout -ObserverProbe $true
    if ($successEvidence.finalEventCount -ne 2 -or
        @($successEvidence.finalEvents).Count -ne 2 -or
        @($successEvidence.finalEventSummaries).Count -ne 2) {
        throw "self-test: success evidence parser did not preserve two event records"
    }
    if (-not $successEvidence.observerRecovery.ok -or
        $successEvidence.observerRecovery.currentFrame -ne 5 -or
        $successEvidence.observerRecovery.snapshotFrame -ne 5 -or
        $successEvidence.observerRecovery.initialSnapshotFrame -ne 0 -or
        $successEvidence.observerRecovery.lastFrame -ne 5 -or
        $successEvidence.observerRecovery.observerLastFrame -ne 5 -or
        $successEvidence.observerRecovery.hash -ne "aabbccddeeff0011") {
        throw "self-test: observer recovery evidence parser failed"
    }
    $successEvidenceJson = $successEvidence | ConvertTo-Json -Depth 20
    $parsedSuccessEvidence = $successEvidenceJson | ConvertFrom-Json
    $roundTripEvents = @($parsedSuccessEvidence.finalEvents)
    $roundTripSummaries = @($parsedSuccessEvidence.finalEventSummaries)
    if ($roundTripEvents.Count -ne 2 -or $roundTripSummaries.Count -ne 2) {
        throw "self-test: success evidence JSON collapsed nested arrays"
    }
    $emptyEventEvidence = Get-ClientSuccessEvidence `
        -Stdout (@(
            "final event count: 0",
            "final events json: []",
            "final event summaries json: []"
        ) -join "`n") `
        -ObserverProbe $false
    if ($emptyEventEvidence.finalEventCount -ne 0 -or
        @($emptyEventEvidence.finalEvents).Count -ne 0 -or
        @($emptyEventEvidence.finalEventSummaries).Count -ne 0 -or
        $null -ne $emptyEventEvidence.observerRecovery) {
        throw "self-test: success evidence parser did not preserve empty event arrays"
    }
    $eventCountMismatchRejected = $false
    try {
        Get-ClientSuccessEvidence `
            -Stdout ($successStdout.Replace("final event count: 2", "final event count: 1")) `
            -ObserverProbe $true | Out-Null
    } catch {
        $eventCountMismatchRejected = $_.Exception.Message -match 'event count mismatch'
    }
    if (-not $eventCountMismatchRejected) {
        throw "self-test: success evidence parser accepted mismatched event count"
    }
    $pidIdentity = @(Get-PidOwnershipIdentities -Items @([pscustomobject]@{
        name = "game-server"; pid = 42; startedAt = "2026-07-10T12:00:00+08:00"
    }))
    $changedPidIdentity = @(Get-PidOwnershipIdentities -Items @([pscustomobject]@{
        name = "redis"; pid = 42; startedAt = "2026-07-10T12:00:00+08:00"
    }))
    if (($pidIdentity -join ",") -eq ($changedPidIdentity -join ",")) { throw "self-test: PID ownership ignored process name" }
    $pidReaderFixtureDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "myserver-lockstep-pid-reader-$([Guid]::NewGuid().ToString('N'))"
    $pidReaderFixturePath = Join-Path $pidReaderFixtureDirectory "dev-stack.pids.json"
    $savedDevStackPidFile = $script:DevStackPidFile
    try {
        New-Item -ItemType Directory -Path $pidReaderFixtureDirectory | Out-Null
        @'
[
  {
    "name": "nats",
    "pid": 41001,
    "filePath": "C:\\project\\MyServer\\bin\\nats-server.exe",
    "startedAt": "2026-07-10T17:47:37.7931194+08:00"
  },
  {
    "name": "game-server",
    "pid": 41002,
    "filePath": "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    "startedAt": "2026-07-10T17:47:42.5346168+08:00"
  }
]
'@ | Set-Content -LiteralPath $pidReaderFixturePath -Encoding UTF8
        $pidReaderRecords = @(Read-DevStackPidRecords -Path $pidReaderFixturePath)
        if ($pidReaderRecords.Count -ne 2) { throw "self-test: PID JSON array reader did not return two records" }
        if ($pidReaderRecords[0].name -ne "nats" -or $pidReaderRecords[1].name -ne "game-server") {
            throw "self-test: PID JSON array reader combined record names"
        }
        if ([string]$pidReaderRecords[0].startedAt -eq [string]$pidReaderRecords[1].startedAt) {
            throw "self-test: PID JSON array reader combined record start times"
        }
        $pidReaderIdentities = @(Get-PidOwnershipIdentities -Items $pidReaderRecords)
        if ($pidReaderIdentities.Count -ne 2 -or
            @($pidReaderIdentities | Where-Object { $_ -match '^nats\|41001\|' }).Count -ne 1 -or
            @($pidReaderIdentities | Where-Object { $_ -match '^game-server\|41002\|' }).Count -ne 1) {
            throw "self-test: PID JSON array reader returned an invalid ownership identity shape"
        }
        $script:DevStackPidFile = $pidReaderFixturePath
        $pidReaderRemoval = Remove-OwnedPidFile -OwnedServices $pidReaderRecords
        if (-not $pidReaderRemoval.removed -or $pidReaderRemoval.reason -ne "matched-owned-pids" -or (Test-Path -LiteralPath $pidReaderFixturePath)) {
            throw "self-test: PID JSON array reader did not preserve removal ownership identities"
        }
    } finally {
        $script:DevStackPidFile = $savedDevStackPidFile
        Remove-Item -LiteralPath $pidReaderFixturePath -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $pidReaderFixtureDirectory -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $pidReaderFixtureDirectory) {
            throw "self-test: PID JSON array reader fixture remains after cleanup"
        }
    }
    $plannedRegistry = [ordered]@{
        status = "planned"
        serviceName = "game-server"
        instanceId = "lockstep-$testRunId"
        confirmedGameServer = $null
    }
    $ownedGameService = [pscustomobject]@{
        name = "game-server"
        pid = 42
        startTimeUtcTicks = 638878176000000000
        startedAt = "2026-07-10T12:00:00+08:00"
    }
    if (Test-RegistryCleanupOwnership -RegistryOwnership $plannedRegistry -OwnedServices @($ownedGameService) -ExpectedInstanceId "lockstep-$testRunId") {
        throw "self-test: planned registry ownership allowed cleanup"
    }
    $ownedRegistry = [ordered]@{
        status = "owned"
        serviceName = "game-server"
        instanceId = "lockstep-$testRunId"
        confirmedGameServer = [pscustomobject]@{
            pid = 42
            startTimeUtcTicks = 638878176000000000
            startedAt = "2026-07-10T12:00:00+08:00"
        }
    }
    if (-not (Test-RegistryCleanupOwnership -RegistryOwnership $ownedRegistry -OwnedServices @($ownedGameService) -ExpectedInstanceId "lockstep-$testRunId")) {
        throw "self-test: confirmed registry ownership rejected cleanup"
    }
    $ownedRegistry.confirmedGameServer.pid = 43
    if (Test-RegistryCleanupOwnership -RegistryOwnership $ownedRegistry -OwnedServices @($ownedGameService) -ExpectedInstanceId "lockstep-$testRunId") {
        throw "self-test: mismatched confirmed game-server identity allowed registry cleanup"
    }
    $missingPidFileRejected = $false
    $missingPidPath = Join-Path ([System.IO.Path]::GetTempPath()) "myserver-lockstep-missing-$([Guid]::NewGuid().ToString('N')).json"
    try {
        Assert-DevStackPidFileCreated -Path $missingPidPath -ExitCode 0 -StdoutPath "unused.stdout" -StderrPath "unused.stderr"
    } catch {
        $missingPidFileRejected = $_.Exception.Message -match 'success without creating the required PID ownership file'
    }
    if (-not $missingPidFileRejected) { throw "self-test: successful dev-stack without a PID file was accepted" }
    $captureTestDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "myserver lockstep native capture $([Guid]::NewGuid().ToString('N'))"
    $captureLauncherPath = Join-Path $captureTestDirectory "launcher.ps1"
    $captureChildRecordPath = Join-Path $captureTestDirectory "child.json"
    $captureStdoutPath = Join-Path $captureTestDirectory "launcher.stdout.log"
    $captureStderrPath = Join-Path $captureTestDirectory "launcher.stderr.log"
    $captureExitStdoutPath = Join-Path $captureTestDirectory "exit-code.stdout.log"
    $captureExitStderrPath = Join-Path $captureTestDirectory "exit-code.stderr.log"
    $captureSignedExitStdoutPath = Join-Path $captureTestDirectory "signed-exit-code.stdout.log"
    $captureSignedExitStderrPath = Join-Path $captureTestDirectory "signed-exit-code.stderr.log"
    $capture259ExitStdoutPath = Join-Path $captureTestDirectory "exit-code-259.stdout.log"
    $capture259ExitStderrPath = Join-Path $captureTestDirectory "exit-code-259.stderr.log"
    $captureChildRecord = $null
    try {
        New-Item -ItemType Directory -Path $captureTestDirectory | Out-Null
        @'
param([string]$ChildRecordPath)
$ErrorActionPreference = "Stop"
$powerShellHost = (Get-Process -Id $PID).Path
$child = Start-Process `
    -FilePath $powerShellHost `
    -ArgumentList @("-NoProfile", "-Command", "Start-Sleep -Seconds 30") `
    -NoNewWindow `
    -PassThru
[pscustomobject]@{
    pid = $child.Id
    processName = $child.ProcessName
    startTimeUtcTicks = $child.StartTime.ToUniversalTime().Ticks
} | ConvertTo-Json -Compress | Set-Content -LiteralPath $ChildRecordPath -Encoding UTF8
Write-Output "launcher-complete"
[Environment]::Exit(23)
'@ | Set-Content -LiteralPath $captureLauncherPath -Encoding UTF8
        $capturePowerShellHost = (Get-Process -Id $PID).Path
        $ordinaryExitCode = Invoke-NativeCaptured `
            -FilePath $capturePowerShellHost `
            -Arguments @("-NoProfile", "-Command", "[Environment]::Exit(17)") `
            -StdoutPath $captureExitStdoutPath `
            -StderrPath $captureExitStderrPath
        if ($ordinaryExitCode -ne 17) { throw "self-test: native capture lost ordinary nonzero exit code" }
        $signedExitCode = Invoke-NativeCaptured `
            -FilePath $capturePowerShellHost `
            -Arguments @("-NoProfile", "-Command", "[Environment]::Exit(-1)") `
            -StdoutPath $captureSignedExitStdoutPath `
            -StderrPath $captureSignedExitStderrPath
        if ($signedExitCode -ne -1) { throw "self-test: native capture lost signed exit code" }
        $exitCode259 = Invoke-NativeCaptured `
            -FilePath $capturePowerShellHost `
            -Arguments @("-NoProfile", "-Command", "[Environment]::Exit(259)") `
            -StdoutPath $capture259ExitStdoutPath `
            -StderrPath $capture259ExitStderrPath
        if ($exitCode259 -ne 259) { throw "self-test: native capture treated an exited process as still active" }
        $captureStopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        $captureExitCode = Invoke-NativeCaptured `
            -FilePath $capturePowerShellHost `
            -Arguments @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $captureLauncherPath, $captureChildRecordPath) `
            -StdoutPath $captureStdoutPath `
            -StderrPath $captureStderrPath
        $captureStopwatch.Stop()
        if ($captureExitCode -ne 23) {
            $captureStdout = if (Test-Path -LiteralPath $captureStdoutPath) { ((Get-Content -LiteralPath $captureStdoutPath -Raw) | Out-String).Trim() } else { "" }
            $captureStderr = if (Test-Path -LiteralPath $captureStderrPath) { ((Get-Content -LiteralPath $captureStderrPath -Raw) | Out-String).Trim() } else { "" }
            throw "self-test: native capture lost launcher exit code (actual=$captureExitCode, stdout=$captureStdout, stderr=$captureStderr, childRecordExists=$(Test-Path -LiteralPath $captureChildRecordPath))"
        }
        if ($captureStopwatch.Elapsed.TotalSeconds -ge 10) { throw "self-test: native capture waited for a long-lived child" }
        if (-not (Test-Path -LiteralPath $captureChildRecordPath)) { throw "self-test: native capture launcher created no child identity" }
        $captureChildRecord = Get-Content -LiteralPath $captureChildRecordPath -Raw | ConvertFrom-Json
        $captureChild = Get-Process -Id ([int]$captureChildRecord.pid) -ErrorAction SilentlyContinue
        if (-not $captureChild) { throw "self-test: native capture waited for the launcher child to exit" }
        if ($captureChild.ProcessName -ne $captureChildRecord.processName -or $captureChild.StartTime.ToUniversalTime().Ticks -ne [long]$captureChildRecord.startTimeUtcTicks) {
            throw "self-test: native capture child identity changed"
        }
        if ((Get-Content -LiteralPath $captureStdoutPath -Raw) -notmatch 'launcher-complete') {
            throw "self-test: native capture stdout redirection failed"
        }
        if ((Get-Item -LiteralPath $captureStderrPath).Length -ne 0) {
            throw "self-test: native capture wrote unexpected launcher stderr"
        }
    } finally {
        $captureCleanupFailure = $null
        if (-not $captureChildRecord -and (Test-Path -LiteralPath $captureChildRecordPath)) {
            try { $captureChildRecord = Get-Content -LiteralPath $captureChildRecordPath -Raw | ConvertFrom-Json } catch {}
        }
        if ($captureChildRecord) {
            $captureChild = Get-Process -Id ([int]$captureChildRecord.pid) -ErrorAction SilentlyContinue
            if ($captureChild) {
                if ($captureChild.ProcessName -ne $captureChildRecord.processName -or $captureChild.StartTime.ToUniversalTime().Ticks -ne [long]$captureChildRecord.startTimeUtcTicks) {
                    $captureCleanupFailure = "self-test: refusing to stop native capture child after identity changed"
                } else {
                    Stop-Process -Id $captureChild.Id -Force
                    if (-not $captureChild.WaitForExit(5000)) { $captureCleanupFailure = "self-test: native capture child cleanup did not finish" }
                }
            }
        }
        foreach ($capturePath in @($captureLauncherPath, $captureChildRecordPath, $captureStdoutPath, $captureStderrPath, $captureExitStdoutPath, $captureExitStderrPath, $captureSignedExitStdoutPath, $captureSignedExitStderrPath, $capture259ExitStdoutPath, $capture259ExitStderrPath)) {
            Remove-Item -LiteralPath $capturePath -Force -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $captureTestDirectory -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $captureTestDirectory) {
            $captureCleanupFailure = "self-test: native capture temp directory remains after cleanup"
        }
        if ($captureChildRecord) {
            $captureChild = Get-Process -Id ([int]$captureChildRecord.pid) -ErrorAction SilentlyContinue
            if ($captureChild -and $captureChild.ProcessName -eq $captureChildRecord.processName -and $captureChild.StartTime.ToUniversalTime().Ticks -eq [long]$captureChildRecord.startTimeUtcTicks) {
                $captureCleanupFailure = "self-test: native capture child remains after cleanup"
            }
        }
        if ($captureCleanupFailure) { throw $captureCleanupFailure }
    }
    $savedRunId = $script:RunId
    $savedRedisUrl = $script:RedisUrl
    $script:RunId = $testRunId
    try {
        $script:RedisUrl = "redis://report-user:report-password@127.0.0.1:6379/3?token=query-secret#fragment-secret"
        $report = New-RunReport -Mode "plan" -Definitions $definitions -ArtifactDirectory ""
        $renderedReport = $report | ConvertTo-Json -Depth 40
        if ($report.schema -ne $ReportSchema -or $report.schemaVersion -ne 1) { throw "self-test: report schema mismatch" }
        if ($report.sideEffects -ne $false -or $report.externalSideEffects -ne $false -or $report.writesArtifacts -ne $false -or $report.networkConnectionsAllowed -ne $false -or $report.ticket.valueRecorded -ne $false) { throw "self-test: plan safety fields mismatch" }
        if ($renderedReport -match 'report-user|report-password|query-secret|fragment-secret') { throw "self-test: report leaked Redis credentials or URL options" }
        if ($renderedReport -notmatch 'redis://127\.0\.0\.1:6379/3') { throw "self-test: redacted Redis endpoint missing" }
        $dryRunReport = New-RunReport -Mode "dry-run" -Definitions @($definitions[0]) -ArtifactDirectory "C:\temp\lockstep"
        if ($dryRunReport.sideEffects -ne $true -or $dryRunReport.externalSideEffects -ne $false -or $dryRunReport.writesArtifacts -ne $true) { throw "self-test: dry-run side effect fields mismatch" }
    } finally {
        $script:RunId = $savedRunId
        $script:RedisUrl = $savedRedisUrl
    }
    return [ordered]@{
        schema = "myserver.lockstep-online-reconcile.self-test.v1"
        ok = $true
        tests = @("parameter-validation", "ephemeral-ticket-secret", "reserved-env-alias", "command-assembly", "mybevy-command-assembly", "mybevy-dual-command-assembly", "mybevy-reconnect-observer-command-assembly", "mybevy-visual-command-assembly", "mybevy-visual-environment", "dry-run-no-ticket", "diagnostic-parser", "success-evidence-parser", "mybevy-telemetry-parser", "mybevy-dual-telemetry-parser", "mybevy-reconnect-observer-telemetry-parser", "mybevy-reconnect-observer-input-rejection", "mybevy-recovery-mismatch-diagnostics", "mybevy-visual-evidence-parser", "pid-ownership-identity", "pid-json-array-reader", "registry-ownership-gate", "missing-pid-file-rejected", "native-signed-exit-code", "native-exit-code-259", "native-launcher-only-wait", "report-schema", "redis-url-redaction")
    }
}

$modeCount = @(@($DryRun, $Execute, $SelfTest) | Where-Object { $_.IsPresent }).Count
if ($modeCount -gt 1) {
    throw "Use only one of -DryRun, -Execute, or -SelfTest."
}

if (-not $RunId) { $RunId = New-RunId }
if (-not $ArtifactRoot) { $ArtifactRoot = Join-Path $ProjectRoot "logs\lockstep-online" }
$checks = @(Get-NormalizedChecks)
$mode = if ($SelfTest) { "self-test" } elseif ($Execute) { "execute" } elseif ($DryRun) { "dry-run" } else { "plan" }
Assert-RunOptions -Mode $mode -Checks $checks

if ($SelfTest) {
    Invoke-SelfTests | ConvertTo-Json -Depth 10
    exit 0
}

$definitions = @(New-StageDefinitions -Checks $checks -CurrentRunId $RunId)
if ($mode -eq "plan") {
    $plan = New-RunReport -Mode "plan" -Definitions $definitions -ArtifactDirectory ""
    $plan["plan"] = [ordered]@{
        executeSwitchRequired = $true
        startsDevStack = [bool]$StartDevStack
        provisionsDevTickets = [bool]$ProvisionDevTickets
        writesFiles = $false
        opensNetworkConnections = $false
        cleanup = "Only exact keys and PIDs owned by this run; no wildcard delete, FLUSH, database reset, or broad stop."
    }
    $plan.endedAt = Get-NowIso
    $plan | ConvertTo-Json -Depth 40
    exit 0
}

$artifactDirectory = Join-Path ([System.IO.Path]::GetFullPath($ArtifactRoot)) $RunId
if (Test-Path -LiteralPath $artifactDirectory) {
    throw "Artifact directory already exists: $artifactDirectory"
}
New-Item -ItemType Directory -Path $artifactDirectory | Out-Null
$report = New-RunReport -Mode $mode -Definitions $definitions -ArtifactDirectory $artifactDirectory
Save-RunReport -Report $report

$ownedServices = @()
$ownedRedisEntries = @()
$runError = $null
$activeStage = $null
$savedEnvironment = @{}
$environmentNamesToRestore = @(
    $RedisRuntimeEnvVar, $TicketEnvVar, $ObserverTicketEnvVar, $TicketSecretEnvVar, "TICKET_SECRET",
    "REDIS_URL", "REDIS_KEY_PREFIX", "REGISTRY_URL", "REGISTRY_KEY_PREFIX",
    "NATS_URL", "DB_ENABLED", "SERVICE_NAME"
) + $MybevyVisualSmokeEnvironmentNames | Select-Object -Unique
foreach ($name in $environmentNamesToRestore) {
    $savedEnvironment[$name] = Get-EnvironmentValue -Name $name
}

try {
    if ($mode -eq "execute") {
        Set-ProcessEnvironmentValue -Name $RedisRuntimeEnvVar -Value $RedisUrl
        $observerRequired = @($definitions | Where-Object { $_.observerProbe -or $_.dualClient -or $_.reconnectObserver }).Count -gt 0

        if ($ProvisionDevTickets) {
            $secret = Get-EnvironmentValue -Name $TicketSecretEnvVar
            if ([string]::IsNullOrWhiteSpace($secret)) {
                $secret = New-EphemeralTicketSecret
                Set-ProcessEnvironmentValue -Name $TicketSecretEnvVar -Value $secret
                $report.ticket.ephemeralSecretGenerated = $true
            }
        } else {
            if ([string]::IsNullOrWhiteSpace((Get-EnvironmentValue -Name $TicketEnvVar))) {
                throw "External ticket environment variable $TicketEnvVar is missing or empty."
            }
            if ($observerRequired -and [string]::IsNullOrWhiteSpace((Get-EnvironmentValue -Name $ObserverTicketEnvVar))) {
                throw "Observer ticket environment variable $ObserverTicketEnvVar is missing or empty."
            }
        }

        if ($StartDevStack) {
            $registryStartInvocationAt = Get-NowIso
            $report.ownership.registry.startInvocationAt = $registryStartInvocationAt
            Save-RunReport -Report $report
            Set-ProcessEnvironmentValue -Name "REDIS_URL" -Value $RedisUrl
            Set-ProcessEnvironmentValue -Name "REDIS_KEY_PREFIX" -Value $RedisKeyPrefix
            Set-ProcessEnvironmentValue -Name "REGISTRY_URL" -Value $RedisUrl
            Set-ProcessEnvironmentValue -Name "REGISTRY_KEY_PREFIX" -Value $RedisKeyPrefix
            Set-ProcessEnvironmentValue -Name "NATS_URL" -Value $LocalNatsUrl
            Set-ProcessEnvironmentValue -Name "DB_ENABLED" -Value "false"
            Set-ProcessEnvironmentValue -Name "SERVICE_NAME" -Value $RegistryServiceName
            if ($ProvisionDevTickets) {
                Set-ProcessEnvironmentValue -Name "TICKET_SECRET" -Value $secret
            }
            $ownedServices = @(
                Start-OwnedDevStack `
                    -ArtifactDirectory $artifactDirectory `
                    -InvocationStartedAt ([DateTime]::Parse($registryStartInvocationAt))
            )
            $confirmedGameServices = @($ownedServices | Where-Object { $_.name -eq "game-server" })
            if ($confirmedGameServices.Count -ne 1) {
                throw "minimal dev-stack ownership confirmation requires exactly one game-server"
            }
            $confirmedGameServer = $confirmedGameServices[0]
            $report.ownership.registry.status = "owned"
            $report.ownership.registry.ownedAt = Get-NowIso
            $report.ownership.registry.confirmedGameServer = [ordered]@{
                pid = [int]$confirmedGameServer.pid
                startedAt = [string]$confirmedGameServer.startedAt
                startTimeUtcTicks = [long]$confirmedGameServer.startTimeUtcTicks
            }
            $report.ownership.services = @($ownedServices)
            $ownedNames = @($ownedServices | ForEach-Object { $_.name })
            foreach ($dependency in $report.dependencies) {
                if ($dependency.name -eq "Redis") { $dependency.ownership = if ($ownedNames -contains "redis") { "started-by-run" } else { "reused" } }
                elseif ($dependency.name -eq "Core NATS") { $dependency.ownership = if ($ownedNames -contains "nats") { "started-by-run" } else { "reused" } }
                elseif ($dependency.name -in @("game-server", "game-server-admin")) { $dependency.ownership = if ($ownedNames -contains "game-server") { "started-by-run" } else { "reused" } }
            }
            Save-RunReport -Report $report
        }

        if ($ProvisionDevTickets) {
            $provision = Invoke-TicketStore -Request @{
                action = "provision"
                runId = $RunId
                keyPrefix = $RedisKeyPrefix
                ttlSeconds = $TicketTtlSeconds
                worldId = $WorldId
                secretEnvVar = $TicketSecretEnvVar
            }
            Set-ProcessEnvironmentValue -Name $TicketEnvVar -Value ([string]$provision.primary.ticket)
            Set-ProcessEnvironmentValue -Name $ObserverTicketEnvVar -Value ([string]$provision.observer.ticket)
            $ownedRedisEntries = @($provision.entries | ForEach-Object {
                [pscustomobject]@{ key = [string]$_.key; expectedValue = [string]$_.expectedValue; kind = [string]$_.kind }
            })
            $report.ticket.primaryFingerprint = [string]$provision.primary.fingerprint
            $report.ticket.observerFingerprint = [string]$provision.observer.fingerprint
            $report.ticket.signatureVerifiedByScript = $true
            $report.ticket.redisBindingsVerified = $true
            $report.ticket.ownedRedisKeys = @($ownedRedisEntries | ForEach-Object {
                [pscustomobject]@{ key = $_.key; kind = $_.kind; expectedValue = $_.expectedValue }
            })
            $report.ticket.validatedRedisKeys = @($report.ticket.ownedRedisKeys)
        } else {
            $ticketRequest = @{
                action = if ($SkipTicketRedisPreflight) { "inspect" } else { "validate-bindings" }
                keyPrefix = $RedisKeyPrefix
                ticketEnvVar = $TicketEnvVar
            }
            if ($observerRequired) { $ticketRequest["observerTicketEnvVar"] = $ObserverTicketEnvVar }
            $ticketCheck = Invoke-TicketStore -Request $ticketRequest
            $report.ticket.primaryFingerprint = [string]$ticketCheck.primary.fingerprint
            if ($ticketCheck.observer) { $report.ticket.observerFingerprint = [string]$ticketCheck.observer.fingerprint }
            $report.ticket.redisBindingsVerified = [bool]$ticketCheck.redisBindingsVerified
            $validatedKeys = @(
                [pscustomobject]@{ key = [string]$ticketCheck.primary.ticketKey; kind = "primary-ticket-owner" },
                [pscustomobject]@{ key = [string]$ticketCheck.primary.versionKey; kind = "primary-ticket-version" }
            )
            if ($ticketCheck.observer) {
                $validatedKeys += @(
                    [pscustomobject]@{ key = [string]$ticketCheck.observer.ticketKey; kind = "observer-ticket-owner" },
                    [pscustomobject]@{ key = [string]$ticketCheck.observer.versionKey; kind = "observer-ticket-version" }
                )
            }
            $report.ticket.validatedRedisKeys = $validatedKeys
        }
        Save-RunReport -Report $report
    }

    foreach ($definition in $definitions) {
        $activeStage = $definition
        $stageResult = Invoke-ClientStage -Definition $definition -Mode $mode -ArtifactDirectory $artifactDirectory
        $report.stages += $stageResult
        Save-RunReport -Report $report
        if ($stageResult.exitCode -ne 0) {
            throw "lockstep stage $($definition.name) failed with exit code $($stageResult.exitCode)"
        }
    }
    $report.status = "passed"
} catch {
    $runError = $_
    $lastStage = @($report.stages | Select-Object -Last 1)
    $lastDiagnostics = if ($lastStage.Count -gt 0) { $lastStage[0].diagnostics } else { $null }
    $report.status = "failed"
    $report.failure = [ordered]@{
        message = $_.Exception.Message
        stage = if ($lastDiagnostics -and $lastDiagnostics.failureStage) { $lastDiagnostics.failureStage } elseif ($lastStage.Count -gt 0) { $lastStage[0].name } elseif ($activeStage) { $activeStage.name } else { "orchestration" }
        roomId = if ($lastStage.Count -gt 0) { $lastStage[0].roomId } elseif ($activeStage) { $activeStage.roomId } else { $null }
        ticketSource = $report.ticket.source
        endpoint = $Server
        frame = if ($lastDiagnostics) { $lastDiagnostics.frame } else { $null }
        serverHash = if ($lastDiagnostics) { $lastDiagnostics.serverHash } else { $null }
        clientHash = if ($lastDiagnostics) { $lastDiagnostics.clientHash } else { $null }
        entityDiff = if ($lastDiagnostics) { $lastDiagnostics.entityDiff } else { $null }
        eventDiff = if ($lastDiagnostics) { $lastDiagnostics.eventDiff } else { $null }
        inputDiff = if ($lastDiagnostics) { $lastDiagnostics.inputDiff } else { $null }
        artifactDirectory = $artifactDirectory
    }
} finally {
    try {
        if ($mode -eq "execute") {
            if ($ownedServices.Count -gt 0 -and $report.ownership.services.Count -eq 0) {
                $report.ownership.services = @($ownedServices)
            }
            $report.cleanup.attempted = $true
            $ownedGameServices = @($ownedServices | Where-Object { $_.name -eq "game-server" })
            $ownedInfrastructure = @($ownedServices | Where-Object { $_.name -ne "game-server" })
            $registryOwnedByRun = Test-RegistryCleanupOwnership `
                -RegistryOwnership $report.ownership.registry `
                -OwnedServices $ownedServices `
                -ExpectedInstanceId "lockstep-$RunId"
            $gameServerStopped = $true

            if ($ownedGameServices.Count -gt 0) {
                $report.cleanup.processes.attempted = $true
                try {
                    $gameResults = @(Stop-OwnedProcesses -OwnedServices $ownedGameServices)
                    $report.cleanup.processes.results += $gameResults
                    $gameServerStopped = @($gameResults | Where-Object { $_.result -notin @("stopped", "already-stopped") }).Count -eq 0
                    if ($gameServerStopped -and ((Test-LocalPortListening -Port $GamePort) -or (Test-LocalPortListening -Port $GameAdminPort))) {
                        $gameServerStopped = $false
                    }
                    if (-not $gameServerStopped) {
                        $report.cleanup.processes.ok = $false
                        Add-CleanupError -Report $report -Stage "stop-game-server" -Message "run-owned game-server did not stop cleanly"
                    }
                } catch {
                    $gameServerStopped = $false
                    $report.cleanup.processes.ok = $false
                    Add-CleanupError -Report $report -Stage "stop-game-server" -Message $_.Exception.Message
                }
            }

            if ($gameServerStopped) {
                if ($ownedRedisEntries.Count -gt 0) {
                    $report.cleanup.redis.attempted = $true
                    try {
                        $cleanup = Invoke-TicketStore -Request @{
                            action = "cleanup"
                            keyPrefix = $RedisKeyPrefix
                            entries = @($ownedRedisEntries)
                        }
                        $report.cleanup.redis.ok = [bool]$cleanup.ok
                        $report.cleanup.redis.results = @($cleanup.results)
                        if (-not $cleanup.ok) {
                            Add-CleanupError -Report $report -Stage "cleanup-ticket-keys" -Message "one or more ticket keys failed compare-delete"
                        }
                    } catch {
                        $report.cleanup.redis.ok = $false
                        $report.cleanup.redis.results = @([pscustomobject]@{ result = "cleanup-error"; message = $_.Exception.Message })
                        Add-CleanupError -Report $report -Stage "cleanup-ticket-keys" -Message $_.Exception.Message
                    }
                }

                if ($registryOwnedByRun) {
                    $report.cleanup.registry.attempted = $true
                    try {
                        $registry = $report.ownership.registry
                        $registryCleanup = Invoke-TicketStore -Request @{
                            action = "cleanup-registry"
                            runId = $RunId
                            keyPrefix = $RedisKeyPrefix
                            serviceName = [string]$registry.serviceName
                            instanceId = [string]$registry.instanceId
                        }
                        $report.cleanup.registry.ok = [bool]$registryCleanup.ok
                        $report.cleanup.registry.results = @($registryCleanup.results)
                        $report.cleanup.registry.guardCode = $registryCleanup.guardCode
                        if (-not $registryCleanup.ok) {
                            $report.cleanup.registry.reason = "ownership-guard-rejected"
                            Add-CleanupError -Report $report -Stage "cleanup-registry" -Message "registry ownership guard rejected deletion"
                        } else {
                            $report.cleanup.registry.reason = "cleaned-owned-registry"
                            $report.ownership.registry.status = "cleaned"
                            $report.ownership.registry.cleanedAt = Get-NowIso
                        }
                    } catch {
                        $report.cleanup.registry.ok = $false
                        $report.cleanup.registry.reason = "cleanup-error"
                        $report.cleanup.registry.results = @([pscustomobject]@{ result = "cleanup-error"; message = $_.Exception.Message })
                        Add-CleanupError -Report $report -Stage "cleanup-registry" -Message $_.Exception.Message
                    }
                } elseif ($StartDevStack) {
                    $report.cleanup.registry.attempted = $false
                    $report.cleanup.registry.ok = $true
                    $report.cleanup.registry.reason = "not-attempted-no-owned-game-server"
                    $report.cleanup.registry.results = @([pscustomobject]@{ result = "not-attempted-no-owned-game-server" })
                }
            } else {
                if ($ownedRedisEntries.Count -gt 0) {
                    $report.cleanup.redis.attempted = $false
                    $report.cleanup.redis.ok = $false
                    $report.cleanup.redis.results = @([pscustomobject]@{ result = "skipped-game-server-still-running" })
                    Add-CleanupError -Report $report -Stage "cleanup-ticket-keys" -Message "skipped while run-owned game-server may still be running"
                }
                if ($registryOwnedByRun) {
                    $report.cleanup.registry.attempted = $false
                    $report.cleanup.registry.ok = $false
                    $report.cleanup.registry.reason = "skipped-game-server-still-running"
                    $report.cleanup.registry.results = @([pscustomobject]@{ result = "skipped-game-server-still-running" })
                    Add-CleanupError -Report $report -Stage "cleanup-registry" -Message "skipped while run-owned game-server may still be running"
                } elseif ($StartDevStack) {
                    $report.cleanup.registry.attempted = $false
                    $report.cleanup.registry.ok = $true
                    $report.cleanup.registry.reason = "not-attempted-no-owned-game-server"
                    $report.cleanup.registry.results = @([pscustomobject]@{ result = "not-attempted-no-owned-game-server" })
                }
            }

            if ($ownedInfrastructure.Count -gt 0) {
                $report.cleanup.processes.attempted = $true
                if ($gameServerStopped) {
                    try {
                        $infrastructureResults = @(Stop-OwnedProcesses -OwnedServices $ownedInfrastructure)
                        $report.cleanup.processes.results += $infrastructureResults
                        if (@($infrastructureResults | Where-Object { $_.result -notin @("stopped", "already-stopped") }).Count -gt 0) {
                            $report.cleanup.processes.ok = $false
                            Add-CleanupError -Report $report -Stage "stop-infrastructure" -Message "one or more run-owned infrastructure processes did not stop"
                        }
                    } catch {
                        $report.cleanup.processes.ok = $false
                        Add-CleanupError -Report $report -Stage "stop-infrastructure" -Message $_.Exception.Message
                    }
                } else {
                    $report.cleanup.processes.ok = $false
                    $report.cleanup.processes.results += @($ownedInfrastructure | ForEach-Object {
                        [pscustomobject]@{ name = $_.name; pid = $_.pid; result = "skipped-game-server-still-running" }
                    })
                    Add-CleanupError -Report $report -Stage "stop-infrastructure" -Message "skipped to avoid stranding a run-owned game-server"
                }
            }

            if ($ownedServices.Count -gt 0) {
                try {
                    $report.cleanup.ports = @(Get-OwnedPortChecks -OwnedServices $ownedServices)
                    if (@($report.cleanup.ports | Where-Object { $_.listeningAfterCleanup }).Count -gt 0) {
                        Add-CleanupError -Report $report -Stage "check-owned-ports" -Message "one or more run-owned service ports are still listening"
                    }
                } catch {
                    Add-CleanupError -Report $report -Stage "check-owned-ports" -Message $_.Exception.Message
                }

                $allOwnedProcessesStopped = @($report.cleanup.processes.results | Where-Object { $_.result -notin @("stopped", "already-stopped") }).Count -eq 0
                if ($allOwnedProcessesStopped) {
                    try {
                        $pidCleanup = Remove-OwnedPidFile -OwnedServices $ownedServices
                        $report.cleanup.pidFile.removed = [bool]$pidCleanup.removed
                        $report.cleanup.pidFile.reason = [string]$pidCleanup.reason
                        if (-not $pidCleanup.removed -and $pidCleanup.reason -ne "not-present") {
                            Add-CleanupError -Report $report -Stage "cleanup-pid-file" -Message "PID ownership changed; file was not removed"
                        }
                    } catch {
                        $report.cleanup.pidFile.reason = "cleanup-error"
                        Add-CleanupError -Report $report -Stage "cleanup-pid-file" -Message $_.Exception.Message
                    }
                } else {
                    $report.cleanup.pidFile.reason = "processes-not-stopped"
                    Add-CleanupError -Report $report -Stage "cleanup-pid-file" -Message "PID file retained because owned processes remain"
                }
            }
        }
    } catch {
        Add-CleanupError -Report $report -Stage "unexpected-cleanup" -Message $_.Exception.Message
    } finally {
        if ($mode -eq "execute") {
            $report.cleanup.environment.attempted = $true
            foreach ($name in $environmentNamesToRestore) {
                try {
                    Set-ProcessEnvironmentValue -Name $name -Value $savedEnvironment[$name]
                } catch {
                    $report.cleanup.environment.ok = $false
                    $report.cleanup.environment.errors += [pscustomobject]@{ name = $name; message = $_.Exception.Message }
                    Add-CleanupError -Report $report -Stage "restore-environment" -Message "failed to restore $name"
                }
            }
        }

        $portsClear = @($report.cleanup.ports | Where-Object { $_.listeningAfterCleanup }).Count -eq 0
        $pidFileSafe = $ownedServices.Count -eq 0 -or $report.cleanup.pidFile.removed -or $report.cleanup.pidFile.reason -eq "not-present"
        $report.cleanup.ok = [bool](
            $report.cleanup.ok -and $report.cleanup.redis.ok -and $report.cleanup.registry.ok -and
            $report.cleanup.processes.ok -and $report.cleanup.environment.ok -and $portsClear -and $pidFileSafe
        )
        if (-not $report.cleanup.ok -and $report.status -eq "passed") {
            $report.status = "failed"
            $report.failure = [ordered]@{
                message = "online checks passed but owned resource cleanup did not complete"
                stage = "cleanup"
                roomId = $null
                ticketSource = $report.ticket.source
                endpoint = $Server
                frame = $null
                serverHash = $null
                clientHash = $null
                entityDiff = $null
                eventDiff = $null
                inputDiff = $null
                artifactDirectory = $artifactDirectory
            }
        }

        $report.endedAt = Get-NowIso
        try {
            Save-RunReport -Report $report
        } catch {
            if (-not $runError) { $runError = $_ }
            Write-Warning "Failed to save final reconciliation report: $($_.Exception.Message)"
        }
    }
}

Write-Host "Lockstep online reconciliation status: $($report.status)"
Write-Host "Report: $($report.logs.report)"
if ($runError -or $report.status -eq "failed") { exit 1 }
exit 0

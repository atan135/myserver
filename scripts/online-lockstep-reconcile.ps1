[CmdletBinding()]
param(
    [Parameter(Mandatory=$false)]
    [ValidateSet("all", "move", "melee", "observer")]
    [string[]]$Check = @("all"),

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
$TicketStorePath = Join-Path $ProjectRoot "tools\lockstep-client\online-ticket-store.mjs"
$DevStackPath = Join-Path $ProjectRoot "scripts\dev-stack.ps1"
$DevStackPidFile = Join-Path $ProjectRoot "logs\dev-stack\dev-stack.pids.json"
$RedisRuntimeEnvVar = "MYSERVER_LOCKSTEP_REDIS_URL_RUNTIME"
$ReportSchema = "myserver.lockstep-online-reconcile.report.v1"
$LocalNatsUrl = "nats://127.0.0.1:4222"
$RegistryServiceName = "game-server"

function Get-NowIso {
    return (Get-Date).ToUniversalTime().ToString("o")
}

function New-RunId {
    $stamp = (Get-Date).ToUniversalTime().ToString("yyyyMMdd-HHmmss")
    $suffix = [Guid]::NewGuid().ToString("N").Substring(0, 8)
    return "$stamp-$suffix".ToLowerInvariant()
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
    )
    foreach ($name in $ticketEnvironmentNames) {
        if ($reservedEnvironmentNames -contains $name) {
            throw "Ticket environment variable $name aliases a reserved runtime variable."
        }
    }
    if ($Checks.Count -eq 0) {
        throw "At least one check is required."
    }
}

function Get-NormalizedChecks {
    $requested = @($Check | ForEach-Object { $_.ToLowerInvariant() })
    if ($requested -contains "all") {
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
            default { throw "Unsupported check: $name" }
        }
    }
    return @($definitions)
}

function New-ClientArguments {
    param([pscustomobject]$Stage, [string]$Mode)
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
        [string]$StderrPath
    )

    $argumentLine = (@($Arguments | ForEach-Object {
        ConvertTo-NativeCommandLineArgument -Argument ([string]$_)
    }) -join " ")
    $startParameters = @{
        FilePath = $FilePath
        WorkingDirectory = $ProjectRoot
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
    }
}

function Read-TextFile {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return "" }
    return Get-Content -LiteralPath $Path -Raw
}

function Invoke-ClientStage {
    param([pscustomobject]$Definition, [string]$Mode, [string]$ArtifactDirectory)
    $stdoutPath = Join-Path $ArtifactDirectory "$($Definition.name).stdout.log"
    $stderrPath = Join-Path $ArtifactDirectory "$($Definition.name).stderr.log"
    $arguments = New-ClientArguments -Stage $Definition -Mode $Mode
    $startedAt = Get-NowIso
    $exitCode = Invoke-NativeCaptured -FilePath "cargo" -Arguments $arguments -StdoutPath $stdoutPath -StderrPath $stderrPath
    $endedAt = Get-NowIso
    $stdout = Read-TextFile -Path $stdoutPath
    $stderr = Read-TextFile -Path $stderrPath
    $diagnostics = Get-ClientDiagnostics -Stdout $stdout -Stderr $stderr
    $finalFrame = $null
    $finalHash = $null
    if ($stdout -match '(?m)^final frame:\s*([0-9]+)') { $finalFrame = [int]$Matches[1] }
    if ($stdout -match '(?m)^final hash:\s*([0-9a-fA-F]+)') { $finalHash = $Matches[1].ToLowerInvariant() }
    $result = [ordered]@{
        name = $Definition.name
        scenario = $Definition.scenario
        roomId = $Definition.roomId
        observerProbe = [bool]$Definition.observerProbe
        status = if ($exitCode -eq 0) { "passed" } else { "failed" }
        exitCode = $exitCode
        startedAt = $startedAt
        endedAt = $endedAt
        finalFrame = $finalFrame
        finalHash = $finalHash
        diagnostics = $diagnostics
        stdout = $stdoutPath
        stderr = $stderrPath
    }
    if ($Definition.observerProbe -and $stdout -match '(?m)^observer hash:\s*([0-9a-fA-F]+)') {
        $result["observerHash"] = $Matches[1].ToLowerInvariant()
    }
    return $result
}

function Invoke-SelfTests {
    $testRunId = "20260710-120000-a1b2c3d4"
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
    $diagnostics = Get-ClientDiagnostics -Stdout "" -Stderr "online mismatch: first mismatch frame 7`nserver_hash: aabb`nclient_hash: ccdd`nentity diffs:`n  entity 1`nevent diffs:`n  events differ`ninputs:`n  move"
    if ($diagnostics.frame -ne 7 -or $diagnostics.serverHash -ne "aabb" -or $diagnostics.clientHash -ne "ccdd") { throw "self-test: diagnostic parser failed" }
    $cleanupDiagnostics = Get-ClientDiagnostics -Stdout "" -Stderr "online cleanup failed: room_end rejected by server: END_FAILED"
    if ($cleanupDiagnostics.failureStage -ne "room_end") { throw "self-test: cleanup failure stage parser failed" }
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
        tests = @("parameter-validation", "reserved-env-alias", "command-assembly", "dry-run-no-ticket", "diagnostic-parser", "pid-ownership-identity", "pid-json-array-reader", "registry-ownership-gate", "missing-pid-file-rejected", "native-signed-exit-code", "native-exit-code-259", "native-launcher-only-wait", "report-schema", "redis-url-redaction")
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
    $RedisRuntimeEnvVar, $TicketEnvVar, $ObserverTicketEnvVar, "TICKET_SECRET",
    "REDIS_URL", "REDIS_KEY_PREFIX", "REGISTRY_URL", "REGISTRY_KEY_PREFIX",
    "NATS_URL", "DB_ENABLED", "SERVICE_NAME"
) | Select-Object -Unique
foreach ($name in $environmentNamesToRestore) {
    $savedEnvironment[$name] = Get-EnvironmentValue -Name $name
}

try {
    if ($mode -eq "execute") {
        Set-ProcessEnvironmentValue -Name $RedisRuntimeEnvVar -Value $RedisUrl
        $observerRequired = @($definitions | Where-Object { $_.observerProbe }).Count -gt 0

        if ($ProvisionDevTickets) {
            $secret = Get-EnvironmentValue -Name $TicketSecretEnvVar
            if ([string]::IsNullOrWhiteSpace($secret)) {
                throw "-$TicketSecretEnvVar must contain the dev ticket signing secret when -ProvisionDevTickets is used."
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

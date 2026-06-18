param(
    [Parameter(Mandatory=$false)]
    [switch]$Stop,

    [Parameter(Mandatory=$false)]
    [switch]$Restart,

    [Parameter(Mandatory=$false)]
    [switch]$StopExistingProjectProcesses,

    [Parameter(Mandatory=$false)]
    [switch]$Status,

    [Parameter(Mandatory=$false)]
    [switch]$NoRedis,

    [Parameter(Mandatory=$false)]
    [switch]$NoNats,

    [Parameter(Mandatory=$false)]
    [switch]$NoAuth,

    [Parameter(Mandatory=$false)]
    [switch]$NoGame,

    [Parameter(Mandatory=$false)]
    [switch]$NoProxy,

    [Parameter(Mandatory=$false)]
    [switch]$NoAdminApi,

    [Parameter(Mandatory=$false)]
    [switch]$NoAdminWeb,

    [Parameter(Mandatory=$false)]
    [switch]$WithChat,

    [Parameter(Mandatory=$false)]
    [switch]$WithMatch,

    [Parameter(Mandatory=$false)]
    [switch]$WithAnnounce,

    [Parameter(Mandatory=$false)]
    [switch]$NoMetricsCollector,

    [Parameter(Mandatory=$false)]
    [switch]$WithMetricsCollector,

    [Parameter(Mandatory=$false)]
    # Local dev stack bind port only. Test/production endpoint discovery must use registry data.
    [int]$GamePort = 7000,

    [Parameter(Mandatory=$false)]
    # Local dev stack admin bind port only. Do not use this as a service discovery fallback.
    [int]$GameAdminPort = 7500,

    [Parameter(Mandatory=$false)]
    [string]$GameInstanceId = "game-server-001",

    [Parameter(Mandatory=$false)]
    [int]$WaitTimeoutSeconds = 180
)

$ErrorActionPreference = "Stop"

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ProjectBin = Join-Path $ProjectRoot "bin"
$LogDir = Join-Path $ProjectRoot "logs\dev-stack"
$PidFile = Join-Path $LogDir "dev-stack.pids.json"
$DefaultRedisPort = 6379
$DefaultNatsPort = 4222
$PowerShellHost = (Get-Process -Id $PID).Path
if (-not $PowerShellHost) {
    $PowerShellHost = "powershell.exe"
}

function Get-NowIso {
    return (Get-Date).ToString("o")
}

function Get-EnvValue {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Path,

        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$false)]
        [string]$Default = ""
    )

    if (-not (Test-Path $Path)) {
        return $Default
    }

    foreach ($line in Get-Content $Path) {
        $trimmed = $line.Trim()
        if ($trimmed.Length -eq 0 -or $trimmed.StartsWith("#")) {
            continue
        }

        $separator = $trimmed.IndexOf("=")
        if ($separator -lt 1) {
            continue
        }

        $key = $trimmed.Substring(0, $separator).Trim()
        if ($key -ne $Name) {
            continue
        }

        return $trimmed.Substring($separator + 1).Trim().Trim('"').Trim("'")
    }

    return $Default
}

function Find-Executable {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$false)]
        [string[]]$CandidatePaths = @()
    )

    foreach ($candidate in $CandidatePaths) {
        if ($candidate -and (Test-Path $candidate)) {
            return (Resolve-Path $candidate).Path
        }
    }

    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    return $null
}

function Test-TcpPort {
    param(
        [Parameter(Mandatory=$true)]
        [string]$HostName,

        [Parameter(Mandatory=$true)]
        [int]$Port,

        [Parameter(Mandatory=$false)]
        [int]$TimeoutMs = 500
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

function Test-PortInUse {
    param(
        [Parameter(Mandatory=$true)]
        [int]$Port
    )

    $tcpConnections = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    if ($tcpConnections) {
        return $true
    }

    $udpEndpoints = Get-NetUDPEndpoint -LocalPort $Port -ErrorAction SilentlyContinue
    return [bool]$udpEndpoints
}

function Wait-TcpPort {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [string]$HostName,

        [Parameter(Mandatory=$true)]
        [int]$Port,

        [Parameter(Mandatory=$false)]
        [int]$ProcessId = 0,

        [Parameter(Mandatory=$true)]
        [int]$TimeoutSeconds
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-TcpPort -HostName $HostName -Port $Port -TimeoutMs 500) {
            Write-Host "$Name is listening on $HostName`:$Port" -ForegroundColor Green
            return $true
        }

        if ($ProcessId -gt 0 -and -not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) {
            throw "$Name exited before opening $HostName`:$Port. Check logs under $LogDir."
        }

        Start-Sleep -Milliseconds 500
    }

    Write-Warning "$Name did not open $HostName`:$Port within $TimeoutSeconds seconds. Check logs under $LogDir."
    return $false
}

function Assert-TcpPort {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [string]$HostName,

        [Parameter(Mandatory=$true)]
        [int]$Port,

        [Parameter(Mandatory=$false)]
        [int]$ProcessId = 0,

        [Parameter(Mandatory=$true)]
        [int]$TimeoutSeconds
    )

    $ready = Wait-TcpPort `
        -Name $Name `
        -HostName $HostName `
        -Port $Port `
        -ProcessId $ProcessId `
        -TimeoutSeconds $TimeoutSeconds

    if (-not $ready) {
        throw "$Name did not open $HostName`:$Port within $TimeoutSeconds seconds. Check logs under $LogDir."
    }
}

function Read-DevStackPids {
    if (-not (Test-Path $PidFile)) {
        return @()
    }

    $content = Get-Content $PidFile -Raw
    if (-not $content.Trim()) {
        return @()
    }

    $items = $content | ConvertFrom-Json
    if ($null -eq $items) {
        return @()
    }

    if ($items -is [array]) {
        return $items
    }

    return @($items)
}

function Get-ProcessChildren {
    param(
        [Parameter(Mandatory=$true)]
        [int]$ParentProcessId
    )

    $children = Get-CimInstance Win32_Process -Filter "ParentProcessId=$ParentProcessId" -ErrorAction SilentlyContinue
    foreach ($child in $children) {
        Get-ProcessChildren -ParentProcessId $child.ProcessId
        $child
    }
}

function Stop-ProcessTree {
    param(
        [Parameter(Mandatory=$true)]
        [int]$ProcessId
    )

    foreach ($child in Get-ProcessChildren -ParentProcessId $ProcessId) {
        Stop-Process -Id $child.ProcessId -Force -ErrorAction SilentlyContinue
    }

    Stop-Process -Id $ProcessId -Force -ErrorAction SilentlyContinue
}

function Get-ListeningProcessIdsOnPort {
    param(
        [Parameter(Mandatory=$true)]
        [int]$Port
    )

    $ids = @()

    $tcpConnections = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    foreach ($connection in $tcpConnections) {
        if ($connection.OwningProcess) {
            $ids += [int]$connection.OwningProcess
        }
    }

    $udpEndpoints = Get-NetUDPEndpoint -LocalPort $Port -ErrorAction SilentlyContinue
    foreach ($endpoint in $udpEndpoints) {
        if ($endpoint.OwningProcess) {
            $ids += [int]$endpoint.OwningProcess
        }
    }

    return @($ids | Sort-Object -Unique)
}

function Get-ProjectProcessIds {
    $processes = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.CommandLine -and
            (
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "scripts\dev-")) -or
                $_.CommandLine -match [regex]::Escape("dev:admin-api") -or
                $_.CommandLine -match [regex]::Escape("dev:admin-web") -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\admin-api")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\admin-web")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\game-server")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\game-proxy")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\chat-server")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\match-service")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\announce-service")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "apps\metrics-collector")) -or
                $_.CommandLine -match [regex]::Escape((Join-Path $ProjectRoot "bin\nats-server.exe"))
            )
        }

    return @($processes | ForEach-Object { [int]$_.ProcessId } | Sort-Object -Unique)
}

function Stop-ListeningProcessesOnPorts {
    param(
        [Parameter(Mandatory=$true)]
        [int[]]$Ports
    )

    $processIds = @()
    foreach ($port in $Ports) {
        $processIds += Get-ListeningProcessIdsOnPort -Port $port
    }

    foreach ($processId in ($processIds | Sort-Object -Unique)) {
        $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($process) {
            Write-Host "Stopping process on target port $($process.ProcessName) (PID $processId)" -ForegroundColor Cyan
            Stop-ProcessTree -ProcessId $processId
        }
    }
}

function Stop-ExistingProjectProcesses {
    $processIds = Get-ProjectProcessIds
    foreach ($processId in $processIds) {
        $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($process) {
            Write-Host "Stopping existing project process $($process.ProcessName) (PID $processId)" -ForegroundColor Cyan
            Stop-ProcessTree -ProcessId $processId
        }
    }
}

function Stop-DevStack {
    $items = Read-DevStackPids
    if ($items.Count -eq 0) {
        Write-Host "No dev stack PID file found." -ForegroundColor Yellow
        return
    }

    foreach ($item in $items) {
        $process = Get-Process -Id $item.pid -ErrorAction SilentlyContinue
        if ($process) {
            Write-Host "Stopping $($item.name) (PID $($item.pid))" -ForegroundColor Cyan
            Stop-ProcessTree -ProcessId ([int]$item.pid)
        } else {
            Write-Host "$($item.name) (PID $($item.pid)) is not running" -ForegroundColor Gray
        }
    }

    Remove-Item $PidFile -Force -ErrorAction SilentlyContinue
}

function Show-DevStackStatus {
    $items = Read-DevStackPids
    if ($items.Count -eq 0) {
        Write-Host "No dev stack PID file found." -ForegroundColor Yellow
        return
    }

    foreach ($item in $items) {
        $process = Get-Process -Id $item.pid -ErrorAction SilentlyContinue
        $state = if ($process) { "running" } else { "stopped" }
        Write-Host ("{0,-20} PID={1,-8} {2}" -f $item.name, $item.pid, $state)
        if ($item.stdout) {
            Write-Host "  stdout: $($item.stdout)" -ForegroundColor Gray
        }
        if ($item.stderr) {
            Write-Host "  stderr: $($item.stderr)" -ForegroundColor Gray
        }
    }
}

function Start-ManagedProcess {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [string]$FilePath,

        [Parameter(Mandatory=$false)]
        [string[]]$Arguments = @(),

        [Parameter(Mandatory=$false)]
        [string]$WorkingDirectory = $ProjectRoot
    )

    New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
    $stdout = Join-Path $LogDir "$Name.out.log"
    $stderr = Join-Path $LogDir "$Name.err.log"

    $process = Start-Process `
        -FilePath $FilePath `
        -ArgumentList $Arguments `
        -WorkingDirectory $WorkingDirectory `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr `
        -PassThru

    Write-Host ("Started {0,-20} PID={1}" -f $Name, $process.Id) -ForegroundColor Cyan

    return [pscustomobject]@{
        name = $Name
        pid = $process.Id
        filePath = $FilePath
        arguments = $Arguments
        workingDirectory = $WorkingDirectory
        stdout = $stdout
        stderr = $stderr
        startedAt = Get-NowIso
    }
}

function Start-PowerShellScript {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [string]$ScriptPath,

        [Parameter(Mandatory=$false)]
        [string[]]$ScriptArguments = @()
    )

    $arguments = @(
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        $ScriptPath
    ) + $ScriptArguments

    return Start-ManagedProcess -Name $Name -FilePath $PowerShellHost -Arguments $arguments -WorkingDirectory $ProjectRoot
}

function Start-NpmScriptIfNeeded {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [string]$ScriptName,

        [Parameter(Mandatory=$false)]
        [string]$HostName = "127.0.0.1",

        [Parameter(Mandatory=$true)]
        [int[]]$RequiredTcpPorts
    )

    $readyPorts = @()
    foreach ($port in $RequiredTcpPorts) {
        if (Test-TcpPort -HostName $HostName -Port $port -TimeoutMs 300) {
            $readyPorts += $port
        }
    }

    if ($readyPorts.Count -eq $RequiredTcpPorts.Count) {
        Write-Host "$Name already has required port(s): $($readyPorts -join ', '); reusing it." -ForegroundColor Green
        return $null
    }

    $blockingPorts = @()
    foreach ($port in ($RequiredTcpPorts | Sort-Object -Unique)) {
        if (Test-PortInUse -Port $port) {
            $blockingPorts += $port
        }
    }

    if ($blockingPorts.Count -gt 0) {
        throw "$Name has partial or conflicting listening port(s): $($blockingPorts -join ', '). Stop stale processes or rerun with -Restart."
    }

    return Start-ManagedProcess `
        -Name $Name `
        -FilePath "cmd.exe" `
        -Arguments @("/d", "/s", "/c", "npm", "run", $ScriptName) `
        -WorkingDirectory $ProjectRoot
}

function Start-ServiceScriptIfNeeded {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [string]$ScriptPath,

        [Parameter(Mandatory=$true)]
        [int[]]$RequiredTcpPorts,

        [Parameter(Mandatory=$false)]
        [int[]]$ConflictPorts = @(),

        [Parameter(Mandatory=$false)]
        [string[]]$ScriptArguments = @()
    )

    $readyPorts = @()
    foreach ($port in $RequiredTcpPorts) {
        if (Test-TcpPort -HostName "127.0.0.1" -Port $port -TimeoutMs 300) {
            $readyPorts += $port
        }
    }

    if ($readyPorts.Count -eq $RequiredTcpPorts.Count) {
        Write-Host "$Name already has required port(s): $($readyPorts -join ', '); reusing it." -ForegroundColor Green
        return $null
    }

    $blockingPorts = @()
    foreach ($port in ($RequiredTcpPorts + $ConflictPorts | Sort-Object -Unique)) {
        if (Test-PortInUse -Port $port) {
            $blockingPorts += $port
        }
    }

    if ($blockingPorts.Count -gt 0) {
        throw "$Name has partial or conflicting listening port(s): $($blockingPorts -join ', '). Stop stale processes or rerun with -Restart."
    }

    return Start-PowerShellScript -Name $Name -ScriptPath $ScriptPath -ScriptArguments $ScriptArguments
}

function Start-InfraIfNeeded {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,

        [Parameter(Mandatory=$true)]
        [int]$Port,

        [Parameter(Mandatory=$true)]
        [string]$ExecutableName,

        [Parameter(Mandatory=$true)]
        [string[]]$CandidatePaths,

        [Parameter(Mandatory=$false)]
        [string[]]$Arguments = @()
    )

    if (Test-TcpPort -HostName "127.0.0.1" -Port $Port -TimeoutMs 300) {
        Write-Host "$Name already listens on 127.0.0.1:$Port; reusing it." -ForegroundColor Green
        return $null
    }

    $executable = Find-Executable -Name $ExecutableName -CandidatePaths $CandidatePaths
    if (-not $executable) {
        throw "$ExecutableName not found. Install it or start $Name manually, then rerun with the matching -No* switch."
    }

    return Start-ManagedProcess -Name $Name -FilePath $executable -Arguments $Arguments -WorkingDirectory $ProjectRoot
}

function Warn-IfDatabaseLooksUnavailable {
    param(
        [Parameter(Mandatory=$true)]
        [string]$ServiceName,

        [Parameter(Mandatory=$true)]
        [string]$EnvPath,

        [Parameter(Mandatory=$false)]
        [switch]$Required
    )

    $dbEnabled = Get-EnvValue -Path $EnvPath -Name "DB_ENABLED" -Default "false"
    if (-not $Required -and $dbEnabled -notin @("true", "TRUE", "True", "1")) {
        return
    }

    $databaseUrl = Get-EnvValue -Path $EnvPath -Name "DATABASE_URL" -Default ""
    $databaseReason = if ($Required) { "requires PostgreSQL" } else { "has DB_ENABLED=true" }

    if ($Required -and $dbEnabled -notin @("true", "TRUE", "True", "1")) {
        Write-Warning "$ServiceName requires PostgreSQL but DB_ENABLED is not true."
    }

    if (-not $databaseUrl) {
        Write-Warning "$ServiceName $databaseReason but DATABASE_URL is empty."
        return
    }

    try {
        $uri = [Uri]$databaseUrl
        $hostName = if ($uri.Host) { $uri.Host } else { "127.0.0.1" }
        $port = if ($uri.Port -gt 0) { $uri.Port } else { 5432 }
        if (-not (Test-TcpPort -HostName $hostName -Port $port -TimeoutMs 500)) {
            Write-Warning "$ServiceName $databaseReason but PostgreSQL is not reachable at $hostName`:$port. Start PostgreSQL separately if this service exits."
        }
    } catch {
        Write-Warning "$ServiceName DATABASE_URL could not be parsed: $databaseUrl"
    }
}

New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

if ($Stop) {
    Stop-DevStack
    exit 0
}

if ($Status) {
    Show-DevStackStatus
    exit 0
}

if ($Restart) {
    Stop-DevStack
    $StopExistingProjectProcesses = $true
}

$existingItems = Read-DevStackPids | Where-Object {
    Get-Process -Id $_.pid -ErrorAction SilentlyContinue
}

if ($existingItems.Count -gt 0) {
    Write-Error "Dev stack processes are already running. Use -Status, -Stop, or -Restart."
}

$authEnv = Join-Path $ProjectRoot "apps\auth-http\.env"
$adminApiEnv = Join-Path $ProjectRoot "apps\admin-api\.env"
$gameEnv = Join-Path $ProjectRoot "apps\game-server\.env"
$proxyEnv = Join-Path $ProjectRoot "apps\game-proxy\.env"

# dev-stack is a manual local launcher. These ports are startup/probe defaults for local processes,
# not test/production service discovery inputs.
$authPort = [int](Get-EnvValue -Path $authEnv -Name "PORT" -Default "3000")
$adminApiPort = [int](Get-EnvValue -Path $adminApiEnv -Name "PORT" -Default "3001")
$adminWebHost = "127.0.0.1"
$adminWebPort = 3002
$proxyPort = [int](Get-EnvValue -Path $proxyEnv -Name "PROXY_PORT" -Default "4000")
$proxyAdminPort = [int](Get-EnvValue -Path $proxyEnv -Name "PROXY_ADMIN_PORT" -Default "7101")
$proxyFallbackPort = [int](Get-EnvValue -Path $proxyEnv -Name "PROXY_TCP_FALLBACK_PORT" -Default ([string]($proxyPort + 10000)))
$authGameProxyPort = [int](Get-EnvValue -Path $authEnv -Name "GAME_PROXY_PORT" -Default "4000")

if ($StopExistingProjectProcesses) {
    Stop-ExistingProjectProcesses
    Stop-ListeningProcessesOnPorts -Ports @(
        $authPort,
        $adminApiPort,
        $adminWebPort,
        $GamePort,
        $GameAdminPort,
        $proxyPort,
        $proxyFallbackPort,
        $proxyAdminPort
    )
    Start-Sleep -Milliseconds 500
}

if ($authGameProxyPort -ne $proxyPort) {
    Write-Warning "auth-http GAME_PROXY_PORT=$authGameProxyPort but game-proxy PROXY_PORT=$proxyPort. Client login may receive the wrong game port."
}

Warn-IfDatabaseLooksUnavailable -ServiceName "auth-http" -EnvPath $authEnv
Warn-IfDatabaseLooksUnavailable -ServiceName "admin-api" -EnvPath $adminApiEnv -Required
Warn-IfDatabaseLooksUnavailable -ServiceName "game-server" -EnvPath $gameEnv

$started = @()
$selectedServices = @()
$serviceProcesses = @{}

try {
    if (-not $NoRedis) {
        $selectedServices += "redis"
        $redisCandidates = @(
            (Join-Path $ProjectBin "redis-server.exe"),
            "C:\Program Files\Redis\redis-server.exe"
        )
        $redisProcess = Start-InfraIfNeeded `
            -Name "redis" `
            -Port $DefaultRedisPort `
            -ExecutableName "redis-server" `
            -CandidatePaths $redisCandidates `
            -Arguments @("--port", [string]$DefaultRedisPort)
        if ($redisProcess) {
            $started += $redisProcess
            $serviceProcesses["redis"] = $redisProcess
        }
    }

    if (-not $NoNats) {
        $selectedServices += "nats"
        $natsCandidates = @((Join-Path $ProjectBin "nats-server.exe"))
        $natsProcess = Start-InfraIfNeeded `
            -Name "nats" `
            -Port $DefaultNatsPort `
            -ExecutableName "nats-server" `
            -CandidatePaths $natsCandidates `
            -Arguments @("-p", [string]$DefaultNatsPort)
        if ($natsProcess) {
            $started += $natsProcess
            $serviceProcesses["nats"] = $natsProcess
        }
    }

    if (-not $NoAuth) {
        $selectedServices += "auth-http"
        $authProcess = Start-ServiceScriptIfNeeded `
            -Name "auth-http" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-auth.ps1") `
            -RequiredTcpPorts @($authPort)
        if ($authProcess) {
            $started += $authProcess
            $serviceProcesses["auth-http"] = $authProcess
        }
    }

    if ($WithMatch) {
        $selectedServices += "match-service"
        $matchProcess = Start-PowerShellScript `
            -Name "match-service" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-match.ps1")
        $started += $matchProcess
        $serviceProcesses["match-service"] = $matchProcess

        $matchPid = if ($serviceProcesses.ContainsKey("match-service")) { [int]$serviceProcesses["match-service"].pid } else { 0 }
        Assert-TcpPort -Name "match-service" -HostName "127.0.0.1" -Port 9002 -ProcessId $matchPid -TimeoutSeconds $WaitTimeoutSeconds
    }

    if (-not $NoGame) {
        $selectedServices += "game-server"
        $gameProcess = Start-ServiceScriptIfNeeded `
            -Name "game-server" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-game.ps1") `
            -RequiredTcpPorts @($GamePort, $GameAdminPort) `
            -ScriptArguments @(
                "-InstanceId", $GameInstanceId,
                "-Port", [string]$GamePort,
                "-AdminPort", [string]$GameAdminPort
            )
        if ($gameProcess) {
            $started += $gameProcess
            $serviceProcesses["game-server"] = $gameProcess
        }
    }

    if (-not $NoProxy) {
        $selectedServices += "game-proxy"
        $proxyProcess = Start-ServiceScriptIfNeeded `
            -Name "game-proxy" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-proxy.ps1") `
            -RequiredTcpPorts @($proxyFallbackPort, $proxyAdminPort) `
            -ConflictPorts @($proxyPort)
        if ($proxyProcess) {
            $started += $proxyProcess
            $serviceProcesses["game-proxy"] = $proxyProcess
        }
    }

    if (-not $NoAdminApi) {
        $selectedServices += "admin-api"
        $adminApiProcess = Start-NpmScriptIfNeeded `
            -Name "admin-api" `
            -ScriptName "dev:admin-api" `
            -RequiredTcpPorts @($adminApiPort)
        if ($adminApiProcess) {
            $started += $adminApiProcess
            $serviceProcesses["admin-api"] = $adminApiProcess
        }
    }

    if (-not $NoAdminWeb) {
        $selectedServices += "admin-web"
        $adminWebProcess = Start-NpmScriptIfNeeded `
            -Name "admin-web" `
            -ScriptName "dev:admin-web" `
            -HostName $adminWebHost `
            -RequiredTcpPorts @($adminWebPort)
        if ($adminWebProcess) {
            $started += $adminWebProcess
            $serviceProcesses["admin-web"] = $adminWebProcess
        }
    }

    if ($WithChat) {
        $selectedServices += "chat-server"
        $chatProcess = Start-PowerShellScript `
            -Name "chat-server" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-chat.ps1")
        $started += $chatProcess
        $serviceProcesses["chat-server"] = $chatProcess
    }

    if ($WithAnnounce) {
        $selectedServices += "announce-service"
        $announceProcess = Start-PowerShellScript `
            -Name "announce-service" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-announce.ps1") `
            -ScriptArguments @("-NoWatch")
        $started += $announceProcess
        $serviceProcesses["announce-service"] = $announceProcess
    }

    if (-not $NoMetricsCollector) {
        $selectedServices += "metrics-collector"
        $metricsCollectorProcess = Start-PowerShellScript `
            -Name "metrics-collector" `
            -ScriptPath (Join-Path $PSScriptRoot "dev-metrics-collector.ps1")
        $started += $metricsCollectorProcess
        $serviceProcesses["metrics-collector"] = $metricsCollectorProcess
    }

    if ($selectedServices.Count -eq 0) {
        Write-Host "No services selected." -ForegroundColor Yellow
        Remove-Item $PidFile -Force -ErrorAction SilentlyContinue
        exit 0
    }

    if ($started.Count -gt 0) {
        $started | ConvertTo-Json -Depth 5 | Set-Content -Path $PidFile -Encoding UTF8
    } else {
        Remove-Item $PidFile -Force -ErrorAction SilentlyContinue
    }

    if (-not $NoRedis) {
        Assert-TcpPort -Name "redis" -HostName "127.0.0.1" -Port $DefaultRedisPort -TimeoutSeconds 10
    }
    if (-not $NoNats) {
        $natsPid = if ($serviceProcesses.ContainsKey("nats")) { [int]$serviceProcesses["nats"].pid } else { 0 }
        Assert-TcpPort -Name "nats" -HostName "127.0.0.1" -Port $DefaultNatsPort -ProcessId $natsPid -TimeoutSeconds 10
    }
    if (-not $NoAuth) {
        $authPid = if ($serviceProcesses.ContainsKey("auth-http")) { [int]$serviceProcesses["auth-http"].pid } else { 0 }
        Assert-TcpPort -Name "auth-http" -HostName "127.0.0.1" -Port $authPort -ProcessId $authPid -TimeoutSeconds $WaitTimeoutSeconds
    }
    if (-not $NoGame) {
        $gamePid = if ($serviceProcesses.ContainsKey("game-server")) { [int]$serviceProcesses["game-server"].pid } else { 0 }
        Assert-TcpPort -Name "game-server" -HostName "127.0.0.1" -Port $GamePort -ProcessId $gamePid -TimeoutSeconds $WaitTimeoutSeconds
        Assert-TcpPort -Name "game-server admin" -HostName "127.0.0.1" -Port $GameAdminPort -ProcessId $gamePid -TimeoutSeconds 30
    }
    if (-not $NoProxy) {
        $proxyPid = if ($serviceProcesses.ContainsKey("game-proxy")) { [int]$serviceProcesses["game-proxy"].pid } else { 0 }
        Assert-TcpPort -Name "game-proxy kcp/tcp bind probe" -HostName "127.0.0.1" -Port $proxyFallbackPort -ProcessId $proxyPid -TimeoutSeconds $WaitTimeoutSeconds
    }
    if (-not $NoAdminApi) {
        $adminApiPid = if ($serviceProcesses.ContainsKey("admin-api")) { [int]$serviceProcesses["admin-api"].pid } else { 0 }
        Assert-TcpPort -Name "admin-api" -HostName "127.0.0.1" -Port $adminApiPort -ProcessId $adminApiPid -TimeoutSeconds $WaitTimeoutSeconds
    }
    if (-not $NoAdminWeb) {
        $adminWebPid = if ($serviceProcesses.ContainsKey("admin-web")) { [int]$serviceProcesses["admin-web"].pid } else { 0 }
        Assert-TcpPort -Name "admin-web" -HostName $adminWebHost -Port $adminWebPort -ProcessId $adminWebPid -TimeoutSeconds $WaitTimeoutSeconds
    }

    Write-Host ""
    Write-Host "Local dev stack started. Endpoints below are local bind/probe defaults, not registry discovery output." -ForegroundColor Green
    if (-not $NoAuth) {
        Write-Host "  auth-http: http://127.0.0.1:$authPort" -ForegroundColor Gray
    }
    if (-not $NoGame) {
        Write-Host "  game-server: 127.0.0.1:$GamePort" -ForegroundColor Gray
    }
    if (-not $NoProxy) {
        Write-Host "  game-proxy: 127.0.0.1:$proxyPort (KCP), 127.0.0.1:$proxyFallbackPort (TCP fallback), 127.0.0.1:$proxyAdminPort (admin)" -ForegroundColor Gray
    }
    if (-not $NoAdminApi) {
        Write-Host "  admin-api: http://127.0.0.1:$adminApiPort" -ForegroundColor Gray
    }
    if (-not $NoAdminWeb) {
        Write-Host "  admin-web: http://$adminWebHost`:$adminWebPort" -ForegroundColor Gray
    }
    if ($WithChat) {
        Write-Host "  chat-server: enabled" -ForegroundColor Gray
    }
    if ($WithMatch) {
        Write-Host "  match-service: enabled" -ForegroundColor Gray
    }
    if ($WithAnnounce) {
        Write-Host "  announce-service: enabled" -ForegroundColor Gray
    }
    if (-not $NoMetricsCollector) {
        Write-Host "  metrics-collector: enabled" -ForegroundColor Gray
    }
    Write-Host "  logs: $LogDir" -ForegroundColor Gray
    Write-Host "  status: powershell -ExecutionPolicy Bypass -File scripts/dev-stack.ps1 -Status" -ForegroundColor Gray
    Write-Host "  stop: powershell -ExecutionPolicy Bypass -File scripts/dev-stack.ps1 -Stop" -ForegroundColor Gray
} catch {
    Write-Host "Startup failed: $_" -ForegroundColor Red
    if ($started.Count -gt 0) {
        Write-Warning "Startup failed; stopping processes started by this run."
        foreach ($item in $started) {
            Stop-ProcessTree -ProcessId ([int]$item.pid)
        }
    }
    throw
}

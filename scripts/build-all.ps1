[CmdletBinding()]
param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [switch]$SkipNodeInstall,
    [switch]$SkipNodeBuild,
    [switch]$SkipRust
)

$ErrorActionPreference = "Stop"

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$logDir = Join-Path $projectRoot "logs"
$logFile = Join-Path $logDir "build.log"
$failures = [System.Collections.Generic.List[string]]::new()

New-Item -ItemType Directory -Force -Path $logDir | Out-Null
Set-Content -LiteralPath $logFile -Value "" -Encoding utf8

function Write-Log {
    param(
        [Parameter(Mandatory)] [string]$Message,
        [string]$Color = "White"
    )

    $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $Message
    Write-Host $line -ForegroundColor $Color
    Add-Content -LiteralPath $logFile -Value $line -Encoding utf8
}

function Invoke-BuildCommand {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [string]$Command,
        [string[]]$Arguments = @(),
        [Parameter(Mandatory)] [string]$WorkingDirectory
    )

    Write-Log "Starting $Name. This operation has no inactivity timeout; wait for its process to exit." "Yellow"
    Push-Location $WorkingDirectory
    try {
        # Native tools routinely write progress and warnings to stderr. Keep those messages,
        # but use only the native process exit code to decide whether the command failed.
        $previousErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            & $Command @Arguments 2>&1 | ForEach-Object { Write-Log "  $_" "Gray" }
            $exitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }

        if ($exitCode -eq 0) {
            Write-Log "$Name succeeded." "Green"
            return $true
        }

        Write-Log "$Name failed with exit code $exitCode." "Red"
        return $false
    } catch {
        Write-Log "$Name could not be started: $($_.Exception.Message)" "Red"
        return $false
    } finally {
        Pop-Location
    }
}

function Get-RustManifests {
    $searchRoots = @(
        (Join-Path $projectRoot "packages"),
        (Join-Path $projectRoot "apps")
    ) | Where-Object { Test-Path -LiteralPath $_ }

    return @(Get-ChildItem -Path $searchRoots -Recurse -File -Filter "Cargo.toml" |
        Where-Object { $_.FullName -notmatch "[\\/]target[\\/]" } |
        Sort-Object FullName)
}

function Get-NodeBuildWorkspaces {
    $rootPackage = Get-Content -Raw -LiteralPath (Join-Path $projectRoot "package.json") | ConvertFrom-Json
    $workspaces = @()

    foreach ($workspacePath in $rootPackage.workspaces) {
        $packagePath = Join-Path $projectRoot (Join-Path $workspacePath "package.json")
        if (-not (Test-Path -LiteralPath $packagePath)) {
            throw "Workspace package.json not found: $workspacePath"
        }

        $package = Get-Content -Raw -LiteralPath $packagePath | ConvertFrom-Json
        if ($null -ne $package.scripts -and $null -ne $package.scripts.build) {
            $workspaces += $package.name
        }
    }

    return $workspaces
}

Write-Log "========================================" "Cyan"
Write-Log "Building all MyServer projects ($Configuration)" "Cyan"
Write-Log "Project root: $projectRoot" "Cyan"
Write-Log "Log file: $logFile" "Cyan"
Write-Log "========================================" "Cyan"

$nodeDependenciesReady = $SkipNodeInstall.IsPresent
if (-not $SkipNodeInstall) {
    if (-not (Invoke-BuildCommand -Name "Node.js workspace dependency installation (npm ci)" -Command "npm" -Arguments @("ci") -WorkingDirectory $projectRoot)) {
        $failures.Add("npm ci")
    } else {
        $nodeDependenciesReady = $true
    }
} else {
    Write-Log "Skipping Node.js dependency installation." "DarkYellow"
}

if ($SkipNodeBuild) {
    Write-Log "Skipping Node.js builds." "DarkYellow"
} elseif (-not $nodeDependenciesReady) {
    Write-Log "Skipping Node.js builds because npm ci did not succeed." "DarkYellow"
} else {
    try {
        $nodeBuildWorkspaces = Get-NodeBuildWorkspaces
        if ($nodeBuildWorkspaces.Count -eq 0) {
            Write-Log "No Node.js workspaces declare a build script." "DarkYellow"
        }

        foreach ($workspace in $nodeBuildWorkspaces) {
            if (-not (Invoke-BuildCommand -Name "Node.js build: $workspace" -Command "npm" -Arguments @("run", "build", "--workspace", $workspace) -WorkingDirectory $projectRoot)) {
                $failures.Add("Node.js build: $workspace")
            }
        }
    } catch {
        Write-Log "Node.js build discovery failed: $($_.Exception.Message)" "Red"
        $failures.Add("Node.js build discovery")
    }
}

if (-not $SkipRust) {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $cargo) {
        Write-Log "cargo was not found in PATH." "Red"
        $failures.Add("Rust build discovery")
    } else {
        $rustManifests = Get-RustManifests
        if ($rustManifests.Count -eq 0) {
            Write-Log "No Cargo.toml files found under apps/ or packages/." "Red"
            $failures.Add("Rust build discovery")
        }

        foreach ($manifest in $rustManifests) {
            $relativeManifest = $manifest.FullName.Substring($projectRoot.Length).TrimStart([char[]]@('\', '/'))
            $arguments = @("build", "--manifest-path", $manifest.FullName)
            if ($Configuration -eq "release") {
                $arguments += "--release"
            }

            if (-not (Invoke-BuildCommand -Name "Rust build: $relativeManifest" -Command $cargo.Source -Arguments $arguments -WorkingDirectory $projectRoot)) {
                $failures.Add("Rust build: $relativeManifest")
            }
        }
    }
} else {
    Write-Log "Skipping Rust builds." "DarkYellow"
}

Write-Log "========================================" "Cyan"
if ($failures.Count -eq 0) {
    Write-Log "All requested initialization and build steps succeeded." "Green"
    exit 0
}

Write-Log "Build completed with $($failures.Count) failed step(s): $($failures -join '; ')" "Red"
exit 1

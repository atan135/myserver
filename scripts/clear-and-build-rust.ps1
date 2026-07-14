[CmdletBinding()]
param(
    [Alias("C")]
    [switch]$Clear,
    [Alias("B")]
    [switch]$Build,
    [Alias("R")]
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$projectRoot = (Resolve-Path "$PSScriptRoot\..").Path
$shouldClear = $Clear.IsPresent
$shouldBuild = $Build.IsPresent

if (-not $shouldClear -and -not $shouldBuild) {
    $shouldClear = $true
    $shouldBuild = $true
}

function Resolve-Cargo {
    $localCargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (Test-Path -LiteralPath $localCargo) {
        return $localCargo
    }

    $cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cargoCommand) {
        return $cargoCommand.Source
    }

    throw "cargo not found. Install Rust or add cargo to PATH."
}

function Invoke-Cargo {
    param(
        [string]$Cargo,
        [string[]]$Arguments,
        [string]$FailureMessage
    )

    & $Cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw $FailureMessage
    }
}

function Get-ProjectRelativePath {
    param([string]$Path)

    $rootPrefix = $projectRoot.TrimEnd([char[]]@("\", "/")) + [System.IO.Path]::DirectorySeparatorChar
    if ($Path.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $Path.Substring($rootPrefix.Length)
    }

    return $Path
}

$cargo = Resolve-Cargo
$searchRoots = @(
    Join-Path $projectRoot "apps"
    Join-Path $projectRoot "packages"
) | Where-Object { Test-Path -LiteralPath $_ }

$manifests = Get-ChildItem -Path $searchRoots -Recurse -File -Filter "Cargo.toml" |
    Where-Object { $_.FullName -notmatch "\\target\\" } |
    Sort-Object FullName

if (-not $manifests) {
    throw "No Cargo.toml files found under apps/ or packages/."
}

$operation = if ($shouldClear -and $shouldBuild) {
    "clear and build"
} elseif ($shouldClear) {
    "clear"
} else {
    "build"
}
$buildMode = if ($Release) { "release" } else { "debug" }

Write-Host "Rust $operation started."
Write-Host "Project root: $projectRoot"
Write-Host "Cargo: $cargo"
Write-Host "Crates: $($manifests.Count)"
if ($shouldBuild) {
    Write-Host "Build mode: $buildMode"
}
Write-Host ""

if ($shouldClear) {
    $cleanManifest = $manifests[0]
    Write-Host "Clearing shared Cargo target directory" -ForegroundColor Yellow
    Invoke-Cargo `
        -Cargo $cargo `
        -Arguments @("clean", "--manifest-path", $cleanManifest.FullName) `
        -FailureMessage "cargo clean failed for shared target directory"

    Write-Host ""
}

if ($shouldBuild) {
    foreach ($manifest in $manifests) {
        $relativeManifest = Get-ProjectRelativePath $manifest.FullName
        $buildArgs = @("build", "--manifest-path", $manifest.FullName)
        if ($Release) {
            $buildArgs += "--release"
        }

        Write-Host "Building $relativeManifest" -ForegroundColor Yellow
        Invoke-Cargo `
            -Cargo $cargo `
            -Arguments $buildArgs `
            -FailureMessage "cargo build failed for $relativeManifest"
    }

    Write-Host ""
}

Write-Host "Rust $operation complete." -ForegroundColor Green

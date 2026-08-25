[CmdletBinding()]
param(
    [switch]$Check
)

$ErrorActionPreference = "Stop"

$projectRoot = Resolve-Path "$PSScriptRoot\.."
$rustfmt = Get-Command rustfmt -ErrorAction SilentlyContinue
if (-not $rustfmt) {
    throw "rustfmt not found in PATH"
}

$csvCodeRoot = (Resolve-Path "$projectRoot\apps\game-server\src\csv_code").Path
$rustFiles = Get-ChildItem -Path "$projectRoot\apps", "$projectRoot\packages" -Recurse -File -Filter "*.rs" |
    Where-Object {
        $fullPath = $_.FullName
        if ($fullPath -match "\\target\\" -or $fullPath -match "\\.tmp\\") {
            return $false
        }
        if ($fullPath.StartsWith($csvCodeRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $false
        }
        $head = Get-Content -LiteralPath $fullPath -TotalCount 5 -ErrorAction SilentlyContinue
        -not (($head -join "`n") -match "@generated")
    }

if ($Check) {
    Write-Host "Checking $($rustFiles.Count) Rust files; generated Rust outputs are skipped."
} else {
    Write-Host "Formatting $($rustFiles.Count) Rust files; generated Rust outputs are skipped."
}

$failedFiles = @()
foreach ($file in $rustFiles) {
    $rustfmtArguments = @("--edition", "2024")
    if ($Check) {
        $rustfmtArguments += "--check"
    }
    $rustfmtArguments += $file.FullName

    & $rustfmt.Source @rustfmtArguments
    if ($LASTEXITCODE -ne 0) {
        if ($Check) {
            $failedFiles += $file.FullName
            continue
        }
        throw "rustfmt failed for $($file.FullName)"
    }
}

if ($Check -and $failedFiles.Count -gt 0) {
    throw "rustfmt check failed for $($failedFiles.Count) file(s): $($failedFiles -join ', ')"
}

if ($Check) {
    Write-Host "Rust format check passed; skipped generated Rust outputs."
} else {
    Write-Host "Formatted $($rustFiles.Count) Rust files; skipped generated Rust outputs."
}

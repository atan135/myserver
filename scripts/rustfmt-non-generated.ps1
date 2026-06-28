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

foreach ($file in $rustFiles) {
    & $rustfmt.Source --edition 2024 $file.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "rustfmt failed for $($file.FullName)"
    }
}

Write-Host "Formatted $($rustFiles.Count) Rust files; skipped generated Rust outputs."

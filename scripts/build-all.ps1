# Build All Servers Script
# Compiles Rust services and installs Node.js dependencies

$ErrorActionPreference = "Continue"

$projectRoot = "$PSScriptRoot\.."
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"

# Setup log file (overwrite mode)
$logDir = "$projectRoot\logs"
if (-not (Test-Path $logDir)) {
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
}
$logFile = "$logDir\build.log"

# Clear log file for overwrite mode
"" | Out-File -FilePath $logFile -Encoding utf8

function Write-Log {
    param([string]$Message, [string]$Color = "White")
    $timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    $line = "[$timestamp] $Message"
    Write-Host $line -ForegroundColor $Color
    $line | Out-File -FilePath $logFile -Append -Encoding utf8
}

Write-Log "========================================" "Cyan"
Write-Log "Building all MyServer services" "Cyan"
Write-Log "========================================" "Cyan"

# 1. Build Rust services
$RustServices = @("game-server", "chat-server", "match-service", "game-proxy")
foreach ($svc in $RustServices) {
    Write-Log "" "White"
    Write-Log "Building Rust service: $svc" "Yellow"
    Push-Location "$projectRoot\apps\$svc"
    try {
        if (Test-Path $cargo) {
            & $cargo build --release 2>&1 | ForEach-Object { Write-Log "  $_" "Gray" }
            if ($LASTEXITCODE -ne 0) {
                Write-Log "  Build failed for $svc" "Red"
            } else {
                Write-Log "  Build succeeded for $svc" "Green"
            }
        } else {
            Write-Log "  cargo.exe not found, skipping $svc" "Gray"
        }
    } finally {
        Pop-Location
    }
}

# 2. Install Node.js services dependencies
Write-Log "" "White"
Write-Log "========================================" "Cyan"
Write-Log "Installing Node.js dependencies" "Cyan"
Write-Log "========================================" "Cyan"

$NodeServices = @("auth-http", "admin-api", "admin-web", "mail-service")
foreach ($svc in $NodeServices) {
    Write-Log "" "White"
    Write-Log "Installing: $svc" "Yellow"
    Push-Location "$projectRoot\apps\$svc"
    try {
        npm install 2>&1 | ForEach-Object { Write-Log "  $_" "Gray" }
        if ($LASTEXITCODE -ne 0) {
            Write-Log "  npm install failed for $svc" "Red"
        } else {
            Write-Log "  npm install succeeded for $svc" "Green"
        }
    } finally {
        Pop-Location
    }
}

Write-Log "" "White"
Write-Log "========================================" "Cyan"
Write-Log "Build complete!" "Cyan"
Write-Log "========================================" "Cyan"
Write-Log "Log saved to: $logFile" "White"

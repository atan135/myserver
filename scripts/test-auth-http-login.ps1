# 登录方式
# powershell -ExecutionPolicy Bypass -File .\scripts\test-auth-http-login.ps1 -LoginName test001 -Password Passw0rd!
# 游客登录
# powershell -ExecutionPolicy Bypass -File .\scripts\test-auth-http-login.ps1 -GuestLogin

param(
  [string]$BaseUrl = "http://127.0.0.1:3000",
  [string]$GuestId = "",
  [string]$LoginName = "",
  [string]$Password = "",
  [switch]$GuestLogin,
  [switch]$SkipIssueTicket
)

$ErrorActionPreference = "Stop"

$useGuestLogin = $GuestLogin -or (-not $LoginName -and -not $Password)
if (-not $useGuestLogin -and (-not $LoginName -or -not $Password)) {
  throw "For account login, both -LoginName and -Password are required."
}

$headers = @{
  "Content-Type" = "application/json"
}

if ($useGuestLogin) {
  if (-not $GuestId) {
    $GuestId = "guest-test-" + [Guid]::NewGuid().ToString("N")
  }

  $loginPath = "/api/v1/auth/guest-login"
  $loginBody = @{
    guestId = $GuestId
  } | ConvertTo-Json
  Write-Host "POST $BaseUrl$loginPath"
} else {
  $loginPath = "/api/v1/auth/login"
  $loginBody = @{
    loginName = $LoginName
    password = $Password
  } | ConvertTo-Json
  Write-Host "POST $BaseUrl$loginPath"
}

$loginResponse = Invoke-RestMethod `
  -Method Post `
  -Uri "$BaseUrl$loginPath" `
  -Headers $headers `
  -Body $loginBody

Write-Host ""
Write-Host "login response:"
$loginResponse | ConvertTo-Json -Depth 10

$authHeaders = @{
  authorization = "Bearer $($loginResponse.accessToken)"
}

Write-Host ""
Write-Host "GET $BaseUrl/api/v1/auth/me"
$meResponse = Invoke-RestMethod `
  -Method Get `
  -Uri "$BaseUrl/api/v1/auth/me" `
  -Headers $authHeaders

Write-Host ""
Write-Host "auth/me response:"
$meResponse | ConvertTo-Json -Depth 10

if (-not $SkipIssueTicket) {
  Write-Host ""
  Write-Host "POST $BaseUrl/api/v1/game-ticket/issue"
  $ticketResponse = Invoke-RestMethod `
    -Method Post `
    -Uri "$BaseUrl/api/v1/game-ticket/issue" `
    -Headers $authHeaders

  Write-Host ""
  Write-Host "game-ticket response:"
  $ticketResponse | ConvertTo-Json -Depth 10
}

# 登录方式
# powershell -ExecutionPolicy Bypass -File .\scripts\test-auth-http-login.ps1 -LoginName test001 -Password Passw0rd!
# 游客登录
# powershell -ExecutionPolicy Bypass -File .\scripts\test-auth-http-login.ps1 -GuestLogin

param(
  [string]$BaseUrl = "http://127.0.0.1:3000",
  [string]$GuestId = "",
  [string]$LoginName = "",
  [string]$Password = "",
  [string]$CharacterId = "",
  [string]$CharacterNamePrefix = "ProbeRole",
  [switch]$GuestLogin,
  [switch]$CreateCharacterIfMissing,
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
  Write-Host "GET $BaseUrl/api/v1/characters"
  $charactersResponse = Invoke-RestMethod `
    -Method Get `
    -Uri "$BaseUrl/api/v1/characters" `
    -Headers $authHeaders

  Write-Host ""
  Write-Host "characters response:"
  $charactersResponse | ConvertTo-Json -Depth 10

  $selectedCharacterId = $CharacterId
  if (-not $selectedCharacterId) {
    $activeCharacters = @($charactersResponse.characters | Where-Object { $_.status -eq "active" })
    if ($activeCharacters.Count -gt 0) {
      $selectedCharacterId = $activeCharacters[0].character_id
    }
  }

  if (-not $selectedCharacterId -and ($CreateCharacterIfMissing -or $useGuestLogin)) {
    $characterName = "$CharacterNamePrefix$((Get-Date).ToUniversalTime().ToString('HHmmss'))"
    $createBody = @{
      name = $characterName
      appearance = @{
        body = "default"
        palette = "blue"
      }
    } | ConvertTo-Json -Depth 10

    Write-Host ""
    Write-Host "POST $BaseUrl/api/v1/characters"
    $createResponse = Invoke-RestMethod `
      -Method Post `
      -Uri "$BaseUrl/api/v1/characters" `
      -Headers $authHeaders `
      -Body $createBody

    Write-Host ""
    Write-Host "character create response:"
    $createResponse | ConvertTo-Json -Depth 10
    $selectedCharacterId = $createResponse.character.character_id
  }

  if (-not $selectedCharacterId) {
    throw "No character selected. Pass -CharacterId or -CreateCharacterIfMissing."
  }

  $selectBody = @{
    character_id = $selectedCharacterId
  } | ConvertTo-Json

  Write-Host ""
  Write-Host "POST $BaseUrl/api/v1/characters/select"
  $ticketResponse = Invoke-RestMethod `
    -Method Post `
    -Uri "$BaseUrl/api/v1/characters/select" `
    -Headers $authHeaders `
    -Body $selectBody

  Write-Host ""
  Write-Host "character select / game-ticket response:"
  $ticketResponse | ConvertTo-Json -Depth 10
}

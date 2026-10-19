#requires -Version 5.1
<#
.SYNOPSIS
  Публикует dev-сборку в rolling prerelease «dev» и пишет manifest.json.
.DESCRIPTION
  Порядок важен для failure-safety: zip -> tar -> manifest, прунинг последним.
  manifest.json описывает КЛИЕНТ (url/sha256/size — его качает лаунчер) и
  сервер (объект server{url,sha256,size} — его качает deploy/zona-update.sh
  на боксе). server_addr (опционально, из repo variable ZONA_SERVER_ADDR)
  попадает как есть — туда тестеры коннектятся кнопкой «Играть».
#>
param(
  [Parameter(Mandatory = $true)][string]$ClientZip,
  [Parameter(Mandatory = $true)][string]$ServerTar,
  [string]$BuildOverride = ""
)

$ErrorActionPreference = 'Stop'
$ReleaseTag = 'dev'
$KeepZips = 2
$KeepTars = 2

function Invoke-Checked {
  param([string]$Exe, [string[]]$GhArgs)
  & $Exe @GhArgs
  if ($LASTEXITCODE -ne 0) {
    throw "$Exe $($GhArgs -join ' ') -> exit $LASTEXITCODE"
  }
}

function Get-AssetApiUrl([string]$Name) {
  $url = gh release view $ReleaseTag --json assets --jq ".assets[] | select(.name==`"$Name`") | .url"
  if ($LASTEXITCODE -ne 0) { throw "gh release view failed ($LASTEXITCODE)" }
  if (-not $url) { throw "asset '$Name' not found in release '$ReleaseTag' after upload" }
  return $url.Trim()
}

foreach ($f in @($ClientZip, $ServerTar)) {
  if (-not (Test-Path $f)) { throw "file not found: $f" }
}

$clientName = Split-Path $ClientZip -Leaf
$serverName = Split-Path $ServerTar -Leaf
$clientHash = (Get-FileHash $ClientZip -Algorithm SHA256).Hash.ToLower()
$serverHash = (Get-FileHash $ServerTar -Algorithm SHA256).Hash.ToLower()
$clientSize = (Get-Item $ClientZip).Length
$serverSize = (Get-Item $ServerTar).Length
if (-not $clientHash -or -not $serverHash) { throw "hash computation failed" }

# 1. rolling prerelease «dev» (создать, если ещё нет)
$null = gh release view $ReleaseTag --json tagName 2>$null
if ($LASTEXITCODE -ne 0) {
  Invoke-Checked gh @('release', 'create', $ReleaseTag, '--prerelease', '--title', 'ZONA dev', '--notes', 'rolling dev builds')
}

# 2. СНАЧАЛА артефакты, манифест — последним (и ссылки на API-URL валидны)
Invoke-Checked gh @('release', 'upload', $ReleaseTag, $ClientZip, '--clobber')
Invoke-Checked gh @('release', 'upload', $ReleaseTag, $ServerTar, '--clobber')

$clientUrl = Get-AssetApiUrl $clientName
$serverUrl = Get-AssetApiUrl $serverName

# 3. manifest.json
$runNumber = 0
if (-not [int]::TryParse($env:GITHUB_RUN_NUMBER, [ref]$runNumber)) { $runNumber = 0 }
$patch = if ($BuildOverride) { $BuildOverride } else { "$runNumber" }
$short = if ($env:GITHUB_SHA -and $env:GITHUB_SHA.Length -ge 7) { $env:GITHUB_SHA.Substring(0, 7) } else { 'local' }
$version = "0.1.$patch"

$manifest = [ordered]@{
  channel    = 'dev'
  version    = $version
  build      = $runNumber
  commit     = $short
  created_at = (Get-Date).ToUniversalTime().ToString('o')
  url        = $clientUrl
  sha256     = $clientHash
  size       = $clientSize
  server     = [ordered]@{
    url    = $serverUrl
    sha256 = $serverHash
    size   = $serverSize
  }
  notes      = "dev-build $short"
}
if ($env:ZONA_SERVER_ADDR) { $manifest['server_addr'] = $env:ZONA_SERVER_ADDR }

$manifest | ConvertTo-Json -Depth 4 | Set-Content -Path manifest.json -Encoding utf8
Invoke-Checked gh @('release', 'upload', $ReleaseTag, 'manifest.json', '--clobber')

# 4. ПРУНИНГ последним: свежие ассеты могут качаться «долгими» апдейтерами
$assets = gh api "repos/$env:GITHUB_REPOSITORY/releases/tags/$ReleaseTag" --paginate --jq '.assets[] | {id: .id, name: .name, created_at: .created_at}' | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "gh api asset list failed ($LASTEXITCODE)" }

$zips = @($assets | Where-Object { $_.name -like 'zona-dev-win-x64.zip' } | Sort-Object created_at -Descending)
$tars = @($assets | Where-Object { $_.name -like 'zona-server-linux-x64.tar.gz' } | Sort-Object created_at -Descending)
$mans = @($assets | Where-Object { $_.name -eq 'manifest.json' } | Sort-Object created_at -Descending)

$toDelete = @()
if ($zips.Count -gt $KeepZips) { $toDelete += $zips[$KeepZips..($zips.Count - 1)] }
if ($tars.Count -gt $KeepTars) { $toDelete += $tars[$KeepTars..($tars.Count - 1)] }
if ($mans.Count -gt 1) { $toDelete += $mans[1..($mans.Count - 1)] }

foreach ($a in $toDelete) {
  Write-Host "prune: $($a.name) ($($a.id))"
  gh api -X DELETE "repos/$env:GITHUB_REPOSITORY/releases/assets/$($a.id)" | Out-Null
  if ($LASTEXITCODE -ne 0) { Write-Warning "prune failed for $($a.name); continuing" }
}

Write-Host "published: version=$version client=$clientName ($clientHash) server=$serverName ($serverHash)"

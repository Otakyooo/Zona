#requires -Version 5.1
<#
.SYNOPSIS
  Публикует dev-сборку в rolling prerelease «dev» и пишет manifest.json.
.DESCRIPTION
  Порядок важен для failure-safety: zip -> tar -> launcher-zip -> manifest,
  прунинг последним.
  manifest.json описывает КЛИЕНТ (url/sha256/size — его качает лаунчер),
  ЛАУНЧЕР (объект launcher{version,url,sha256,size} — M6-8, обновление
  лаунчера отдельно от игры) и сервер (объект server{url,sha256,size} — его
  качает deploy/zona-update.sh на боксе). server_addr (опционально, из repo
  variable ZONA_SERVER_ADDR) попадает как есть — туда тестеры коннектятся
  кнопкой «Играть». Без -LauncherZip блок launcher{} в манифест не пишется
  (лаунчер тогда обновляется только бандлом из клиентского zip).
#>
param(
  [Parameter(Mandatory = $true)][string]$ClientZip,
  [Parameter(Mandatory = $true)][string]$ServerTar,
  [string]$LauncherZip = "",
  [string]$BuildOverride = ""
)

$ErrorActionPreference = 'Stop'
$ReleaseTag = 'dev'
$KeepZips = 2
$KeepTars = 2
$KeepLaunchers = 2

function Invoke-Checked {
  param([string]$Exe, [string[]]$GhArgs)
  & $Exe @GhArgs
  if ($LASTEXITCODE -ne 0) {
    throw "$Exe $($GhArgs -join ' ') -> exit $LASTEXITCODE"
  }
}

function Get-AssetApiUrl([string]$Name) {
  # Именно API-URL ассета (api.github.com/.../assets/ID): web-URL вида
  # github.com/.../releases/download/... для ПРИВАТНОГО репо отдаёт 404
  # при любой схеме авторизации — по ним качают лаунчер и zona-update.sh.
  $url = gh api "repos/{owner}/{repo}/releases/tags/$ReleaseTag" --jq ".assets[] | select(.name==`"$Name`") | .url"
  if ($LASTEXITCODE -ne 0) { throw "gh api releases/tags failed ($LASTEXITCODE)" }
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

$launcherName = ""
$launcherHash = ""
$launcherSize = 0
if ($LauncherZip) {
  foreach ($f in @($LauncherZip)) {
    if (-not (Test-Path $f)) { throw "file not found: $f" }
  }
  $launcherName = Split-Path $LauncherZip -Leaf
  $launcherHash = (Get-FileHash $LauncherZip -Algorithm SHA256).Hash.ToLower()
  $launcherSize = (Get-Item $LauncherZip).Length
  if (-not $launcherHash) { throw "launcher hash computation failed" }
}

# 1. rolling prerelease «dev» (создать, если ещё нет)
$null = gh release view $ReleaseTag --json tagName 2>$null
if ($LASTEXITCODE -ne 0) {
  Invoke-Checked gh @('release', 'create', $ReleaseTag, '--prerelease', '--title', 'ZONA dev', '--notes', 'rolling dev builds')
}

# 2. СНАЧАЛА артефакты, манифест — последним (и ссылки на API-URL валидны)
Invoke-Checked gh @('release', 'upload', $ReleaseTag, $ClientZip, '--clobber')
Invoke-Checked gh @('release', 'upload', $ReleaseTag, $ServerTar, '--clobber')
if ($LauncherZip) {
  Invoke-Checked gh @('release', 'upload', $ReleaseTag, $LauncherZip, '--clobber')
}

$repo = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "Otakyooo/Zona" }
# Прямые web-URL скачивания релиза: для публичного репо они отдаются через CDN
# без анонимного лимита api.github.com (60 req/hr -> HTTP 403).
$clientUrl = "https://github.com/$repo/releases/download/$ReleaseTag/$clientName"
$serverUrl = "https://github.com/$repo/releases/download/$ReleaseTag/$serverName"
$launcherUrl = if ($LauncherZip) { "https://github.com/$repo/releases/download/$ReleaseTag/$launcherName" } else { "" }

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
if ($LauncherZip) {
  # лаунчер того же релиза: свой ассет и своя версия (M6-8)
  $manifest['launcher'] = [ordered]@{
    version = $version
    url     = $launcherUrl
    sha256  = $launcherHash
    size    = $launcherSize
  }
}
# Адрес из переменной репозитория подчищаем: вставка в UI GitHub нередко
# приносит `\r\n`, и `traded-convenient.tun.ply.gg:11114\r\n` уезжает в
# манифест — лаунчер тогда не узнаёт host:port и пускает тестеров в offline.
if ($env:ZONA_SERVER_ADDR) { $manifest['server_addr'] = $env:ZONA_SERVER_ADDR.Trim() }

# utf8 БЕЗ BOM: PS 5.1 при -Encoding utf8 добавляет BOM, и тогда ни
# лаунчер (serde_json), ни бокс (python json) манифест не разберут.
$manifestJson = $manifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText("$PWD/manifest.json", $manifestJson, (New-Object System.Text.UTF8Encoding($false)))
Invoke-Checked gh @('release', 'upload', $ReleaseTag, 'manifest.json', '--clobber')

# 4. ПРУНИНГ последним: свежие ассеты могут качаться «долгими» апдейтерами
$assets = gh api "repos/$env:GITHUB_REPOSITORY/releases/tags/$ReleaseTag" --paginate --jq '.assets[] | {id: .id, name: .name, created_at: .created_at}' | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "gh api asset list failed ($LASTEXITCODE)" }

$zips = @($assets | Where-Object { $_.name -like 'zona-dev-win-x64.zip' } | Sort-Object created_at -Descending)
$tars = @($assets | Where-Object { $_.name -like 'zona-server-linux-x64.tar.gz' } | Sort-Object created_at -Descending)
$launchers = @($assets | Where-Object { $_.name -like 'zona-launcher*x64.zip' } | Sort-Object created_at -Descending)
$mans = @($assets | Where-Object { $_.name -eq 'manifest.json' } | Sort-Object created_at -Descending)

$toDelete = @()
if ($zips.Count -gt $KeepZips) { $toDelete += $zips[$KeepZips..($zips.Count - 1)] }
if ($tars.Count -gt $KeepTars) { $toDelete += $tars[$KeepTars..($tars.Count - 1)] }
if ($launchers.Count -gt $KeepLaunchers) { $toDelete += $launchers[$KeepLaunchers..($launchers.Count - 1)] }
if ($mans.Count -gt 1) { $toDelete += $mans[1..($mans.Count - 1)] }

foreach ($a in $toDelete) {
  Write-Host "prune: $($a.name) ($($a.id))"
  gh api -X DELETE "repos/$env:GITHUB_REPOSITORY/releases/assets/$($a.id)" | Out-Null
  if ($LASTEXITCODE -ne 0) { Write-Warning "prune failed for $($a.name); continuing" }
}

$summary = "published: version=$version client=$clientName ($clientHash) server=$serverName ($serverHash)"
if ($LauncherZip) { $summary += " launcher=$launcherName ($launcherHash)" }
Write-Host $summary

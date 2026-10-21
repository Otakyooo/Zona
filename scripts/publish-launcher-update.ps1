#requires -Version 5.1
<#
.SYNOPSIS
  Out-of-band хотфикс лаунчера (M6-8): грузит лаунчер-зип в rolling
  prerelease «dev» и правит ТОЛЬКО блок launcher{} в manifest.json.
.DESCRIPTION
  Когда нужно выкатить игрокам новую версию ЛАУНЧЕРА, не пересобирая и не
  перекачивая игру: этим скриптом, без CI-прогона. Клиентский zip, серверный
  tar, server_addr и прочие поля манифеста не трогаются.

  Версия в -LauncherVersion обязана быть строго новее той, что уже в
  манифесте (иначе лаунчеры игроков сочли бы апдейт неактуальным и ничего
  не скачали). Обычно это 0.1.<число>, заведомо большее номеров прогонов CI
  (0.1.900+ и выше).

  Что делают игроки: ничего. При следующем старте лаунчер сверяет свою
  версию с manifest.launcher.version, качает зип, проверяет sha256,
  подменяет себя и перезапускается.

  GH_TOKEN должен быть в окружении и иметь права на Releases
  (классический PAT с repo; fine-grained Contents: read НЕ подходит —
  он только читает).
#>
param(
  [Parameter(Mandatory = $true)][string]$LauncherVersion,
  [string]$LauncherZip = "",
  # Собрать лаунчер локально (cargo build --release -p zn-launcher) и
  # упаковать зип самим. Иначе нужен готовый -LauncherZip.
  [switch]$Build,
  [string]$Repo = "Otakyooo/Zona"
)

$ErrorActionPreference = 'Stop'
$ReleaseTag = 'dev'

function Invoke-Checked {
  param([string]$Exe, [string[]]$GhArgs)
  & $Exe @GhArgs
  if ($LASTEXITCODE -ne 0) {
    throw "$Exe $($GhArgs -join ' ') -> exit $LASTEXITCODE"
  }
}

# 0. Версия — семвер и строго новее той, что в манифесте.
if ($LauncherVersion -notmatch '^\d+\.\d+\.\d+$') {
  throw "LauncherVersion должен быть semver (0.1.901), получено '$LauncherVersion'"
}

$assetId = gh api "repos/$Repo/releases/tags/$ReleaseTag" --jq ".assets[] | select(.name==`"manifest.json`") | .id"
if ($LASTEXITCODE -ne 0) { throw "gh api releases/tags failed ($LASTEXITCODE)" }
if (-not $assetId) { throw "asset 'manifest.json' не найден в релизе '$ReleaseTag'" }

$current = gh api "repos/$Repo/releases/assets/$($assetId.Trim())" -H "Accept: application/vnd.github.raw+json"
if ($LASTEXITCODE -ne 0) { throw "gh api asset download failed ($LASTEXITCODE)" }
$manifest = $current | ConvertFrom-Json

$currentLauncherVersion = "0.0.0"
if ($manifest.PSObject.Properties.Name -contains 'launcher' -and $manifest.launcher.version) {
  $currentLauncherVersion = $manifest.launcher.version
}
if ([version]$LauncherVersion -le [version]$currentLauncherVersion) {
  throw "версия $LauncherVersion не новее текущей в манифесте ($currentLauncherVersion) — игрокам не прилетит"
}
Write-Host "лаунчер в манифесте: $currentLauncherVersion -> $LauncherVersion"

# 1. Зип: готовый или собранный на месте.
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("zona-launcher-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $staging | Out-Null
try {
  if ($Build) {
    if ($LauncherZip) { throw "-Build и -LauncherZip взаимоисключающие" }
    Write-Host "собираем zn-launcher (release)…"
    Invoke-Checked cargo @('build', '--release', '-p', 'zn-launcher')
    $stage = Join-Path $staging "zona-launcher"
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item "target/release/zn-launcher.exe" $stage/
    $LauncherZip = Join-Path $staging ("zona-launcher-$LauncherVersion-x64.zip")
    Compress-Archive -Path $stage -DestinationPath $LauncherZip -Force
  }
  if (-not $LauncherZip) { throw "нужен -LauncherZip <путь> или -Build" }
  if (-not (Test-Path $LauncherZip)) { throw "file not found: $LauncherZip" }

  $launcherHash = (Get-FileHash $LauncherZip -Algorithm SHA256).Hash.ToLower()
  $launcherSize = (Get-Item $LauncherZip).Length
  if (-not $launcherHash) { throw "hash computation failed" }

  # 2. Грузим ассет (clobber) — имена разнесены по версии, прунинг
  #    publish-dev-build.ps1 ловит их по маске zona-launcher*x64.zip.
  Invoke-Checked gh @('release', 'upload', $ReleaseTag, $LauncherZip, '--clobber')

  # 3. API-URL ассета.
  $launcherName = Split-Path $LauncherZip -Leaf
  $launcherUrl = gh api "repos/$Repo/releases/tags/$ReleaseTag" --jq ".assets[] | select(.name==`"$launcherName`") | .url"
  if ($LASTEXITCODE -ne 0) { throw "gh api releases/tags failed ($LASTEXITCODE)" }
  if (-not $launcherUrl) { throw "asset '$launcherName' не найден в релизе после upload" }
  $launcherUrl = $launcherUrl.Trim()

  # 4. Правим ТОЛЬКО блок launcher{}.
  $launcherBlock = [ordered]@{
    version = $LauncherVersion
    url     = $launcherUrl
    sha256  = $launcherHash
    size    = $launcherSize
  }
  if ($manifest.PSObject.Properties.Name -contains 'launcher') {
    $manifest.launcher = [PSCustomObject]$launcherBlock
  } else {
    $manifest | Add-Member -NotePropertyName launcher -NotePropertyValue ([PSCustomObject]$launcherBlock)
  }

  # utf8 БЕЗ BOM (иначе ни лаунчер, ни бокс манифест не разберут).
  $manifestJson = $manifest | ConvertTo-Json -Depth 4
  [System.IO.File]::WriteAllText((Join-Path $staging "manifest.json"), $manifestJson, (New-Object System.Text.UTF8Encoding($false)))
  Invoke-Checked gh @('release', 'upload', $ReleaseTag, (Join-Path $staging "manifest.json"), '--clobber')

  Write-Host "готово: лаунчер $LauncherVersion ($launcherHash) — игроки подхватят при следующем старте лаунчера"
}
finally {
  Remove-Item $staging -Recurse -Force
}

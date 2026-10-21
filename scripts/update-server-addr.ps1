#requires -Version 5.1
<#
.SYNOPSIS
  Правит manifest.server_addr в rolling prerelease «dev» с машины dev'а.
.DESCRIPTION
  Когда меняется публичный адрес сервера (пересоздали туннель playit,
  переехали на проброс портов и т.п.) — этим скриптом, без доступа к боксу.
  Пустой -ServerAddr удаляет поле: лаунчер уведёт тестеров в offline.
  GH_TOKEN должен быть в окружении (gh auth status).
#>
param(
  [Parameter(Mandatory = $true)]
  [AllowEmptyString()]
  [string]$ServerAddr,
  # Фингрейнд PAT с Contents: read не умеет релизные ассеты на запись —
  # здесь нужен токен с правами на Releases (например, классический repo).
  [string]$Repo = "Otakyooo/Zona"
)

$ErrorActionPreference = 'Stop'
$ReleaseTag = 'dev'

# Вставленный адрес мог приехать с `\r\n`/пробелами — иначе они уедут в
# манифест, и лаунчер отвергнет значение как не-host:port (offline-режим).
$ServerAddr = $ServerAddr.Trim()

function Invoke-Checked {
  param([string]$Exe, [string[]]$GhArgs)
  & $Exe @GhArgs
  if ($LASTEXITCODE -ne 0) {
    throw "$Exe $($GhArgs -join ' ') -> exit $LASTEXITCODE"
  }
}

# 1. Ищем asset manifest.json в релизе «dev».
$assetId = gh api "repos/$Repo/releases/tags/$ReleaseTag" --jq ".assets[] | select(.name==`"manifest.json`") | .id"
if ($LASTEXITCODE -ne 0) { throw "gh api releases/tags failed ($LASTEXITCODE)" }
if (-not $assetId) { throw "asset 'manifest.json' не найден в релизе '$ReleaseTag'" }

# 2. Качаем текущий манифест (raw).
$current = gh api "repos/$Repo/releases/assets/$($assetId.Trim())" -H "Accept: application/vnd.github.raw+json"
if ($LASTEXITCODE -ne 0) { throw "gh api asset download failed ($LASTEXITCODE)" }

# 3. Правим server_addr (пустая строка — удалить поле).
$manifest = $current | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace($ServerAddr)) {
  if ($manifest.PSObject.Properties.Name -contains 'server_addr') {
    $manifest.PSObject.Properties.Remove('server_addr')
  }
  Write-Host "server_addr удалён из манифеста"
} else {
  if (-not $manifest.PSObject.Properties.Name -contains 'server_addr') {
    $manifest | Add-Member -NotePropertyName server_addr -NotePropertyValue $ServerAddr
  } else {
    $manifest.server_addr = $ServerAddr
  }
  Write-Host "server_addr = $ServerAddr"
}

# 4. Заливаем обратно (clobber).
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("zona-addr-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $staging | Out-Null
try {
  $path = Join-Path $staging "manifest.json"
  # utf8 БЕЗ BOM: под PS 5.1 -Encoding utf8 пишет с BOM, и тогда ни лаунчер
  # (serde_json), ни бокс (python json) манифест не разберут.
  $manifest | ConvertTo-Json -Depth 4 | ForEach-Object {
    [System.IO.File]::WriteAllText($path, $_, (New-Object System.Text.UTF8Encoding($false)))
  }
  Invoke-Checked gh @('release', 'upload', $ReleaseTag, $path, '--clobber', '-R', $Repo)
} finally {
  Remove-Item $staging -Recurse -Force
}
Write-Host "готово: тестеры подхватят адрес при следующей проверке обновлений"

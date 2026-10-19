#!/usr/bin/env bash
# Pull-обновление zn-server из GitHub Releases (приватный репо Otakyooo/ai_games).
#
# CI в LAN не заходит — бокс тянет обновления САМ (pull-модель), это экономит
# минуты Actions и не требует входящих портов. Алгоритм:
#   manifest.json (rolling prerelease «dev») → сверка версии с /opt/zona/installed
#   → tar.gz → SHA-256 → content + zn-server.new → подмена (.bak) →
#   systemctl restart zona-server → health-check (порт 7700 слушается) →
#   промах health-check → откат .bak.
# `/opt/zona/installed` (простой текстовый файл версии) — состояние бокса;
# installed.json лаунчера не трогаем.
#
# Зависимости: bash, curl, python3 (разбор JSON), tar, sha256sum, ss (iproute2).
# Логи: journalctl -u zona-update -n 50

set -euo pipefail

REPO="${ZONA_REPO:-Otakyooo/ai_games}"
TAG="dev"
MANIFEST_ASSET="manifest.json"
ZONA_DIR="${ZONA_DIR:-/opt/zona}"
SECRETS_FILE="${ZONA_SECRETS:-/etc/zona/update.env}"
INSTALLED_FILE="${ZONA_DIR}/installed"

log() { echo "[zona-update] $*"; }
die() { log "ОШИБКА: $*"; exit 1; }

[ -r "$SECRETS_FILE" ] || die "нет доступа к ${SECRETS_FILE} (должен быть root-only, с ZONA_PAT)"
# shellcheck disable=SC1090
source "$SECRETS_FILE"
[ -n "${ZONA_PAT:-}" ] || die "ZONA_PAT не задан в ${SECRETS_FILE}"

for tool in curl python3 tar sha256sum; do
  command -v "$tool" >/dev/null || die "нужен ${tool}"
done

BASE="https://github.com/${REPO}/releases/download/${TAG}"
auth=(-H "Authorization: Bearer ${ZONA_PAT}" -H "User-Agent: zona-update")

log "качаем ${BASE}/${MANIFEST_ASSET}"
manifest_json="$(curl -fsSL "${auth[@]}" "${BASE}/${MANIFEST_ASSET}")" \
  || die "не удалось скачать manifest.json (проверьте ZONA_PAT и релиз '${TAG}')"

fields="$(printf '%s' "$manifest_json" | python3 -c 'import json,sys; m=json.load(sys.stdin); print(m["version"], m["server"]["url"], m["server"]["sha256"])')" \
  || die "manifest.json не разобран"
read -r m_version m_url m_sha <<<"$fields"
[ -n "$m_version" ] && [ -n "$m_url" ] && [ -n "$m_sha" ] || die "в манифесте пустые version/server.url/server.sha256"

installed=""
[ -f "$INSTALLED_FILE" ] && installed="$(tr -d '[:space:]' < "$INSTALLED_FILE")"
if [ -n "$installed" ]; then
  newest="$(printf '%s\n%s\n' "$installed" "$m_version" | sort -V | tail -n1)"
  if [ "$newest" = "$installed" ]; then
    log "актуальная версия ${installed} — обновление не нужно"
    exit 0
  fi
fi
log "доступна версия ${m_version} (установлена ${installed:-нет})"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

log "качаем серверный архив"
curl -fsSL "${auth[@]}" -H "Accept: application/octet-stream" -o "${tmp}/server.tar.gz" "$m_url" \
  || die "не удалось скачать tar.gz (${m_url})"
echo "${m_sha}  ${tmp}/server.tar.gz" | sha256sum -c - >/dev/null \
  || die "SHA-256 не совпал — архив повреждён, установка отменена"

tar -xzf "${tmp}/server.tar.gz" -C "$tmp"
new_bin="${tmp}/zona-server-linux/zn-server"
new_content="${tmp}/zona-server-linux/content"
[ -f "$new_bin" ] || die "в архиве нет zn-server (ожидалась папка zona-server-linux/)"

mkdir -p "${ZONA_DIR}/data"

# Подмена контента: content.new → content (.bak до следующей удачи).
if [ -d "$new_content" ]; then
  rm -rf "${ZONA_DIR}/content.new"
  cp -r "$new_content" "${ZONA_DIR}/content.new"
  rm -rf "${ZONA_DIR}/content.bak"
  [ -d "${ZONA_DIR}/content" ] && mv "${ZONA_DIR}/content" "${ZONA_DIR}/content.bak"
  mv "${ZONA_DIR}/content.new" "${ZONA_DIR}/content"
fi

# Подмена бинарника: атомарный rename в пределах /opt/zona, .bak — для откатa.
cp "$new_bin" "${ZONA_DIR}/zn-server.new"
chmod +x "${ZONA_DIR}/zn-server.new"
rm -f "${ZONA_DIR}/zn-server.bak"
[ -f "${ZONA_DIR}/zn-server" ] && mv "${ZONA_DIR}/zn-server" "${ZONA_DIR}/zn-server.bak"
mv "${ZONA_DIR}/zn-server.new" "${ZONA_DIR}/zn-server"

printf '%s\n' "$m_version" > "$INSTALLED_FILE"
log "перезапуск zona-server"
systemctl restart zona-server

# Health-check: порт 7700 слушается (или сервис активен, если ss недоступен).
ok=0
for _ in $(seq 1 10); do
  if ss -H -lun 2>/dev/null | grep -q ':7700 '; then ok=1; break; fi
  if ! command -v ss >/dev/null && systemctl is-active --quiet zona-server; then ok=1; break; fi
  sleep 1
done
if [ "$ok" != 1 ]; then
  log "health-check не прошёл — откат на ${installed:-предыдущую версию}"
  systemctl stop zona-server || true
  [ -f "${ZONA_DIR}/zn-server.bak" ] && mv "${ZONA_DIR}/zn-server.bak" "${ZONA_DIR}/zn-server"
  [ -d "${ZONA_DIR}/content.bak" ] && { rm -rf "${ZONA_DIR}/content"; mv "${ZONA_DIR}/content.bak" "${ZONA_DIR}/content"; }
  if [ -n "$installed" ]; then printf '%s\n' "$installed" > "$INSTALLED_FILE"; else rm -f "$INSTALLED_FILE"; fi
  systemctl restart zona-server || true
  die "откатились; смотрите journalctl -u zona-server"
fi

rm -f "${ZONA_DIR}/zn-server.bak" "${ZONA_DIR}/content.bak"
log "обновлено до ${m_version}"

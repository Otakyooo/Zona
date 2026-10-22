#!/bin/bash
# Ночная копия БД аккаунтов ZONA (M5-3): онлайн-бэкап через SQLite Backup API.
# Сервер работать может — читается согласованный снапшот, запись в приёмник
# на время копии блокируется (доли секунды). Хранится 7 суток.
#
# Зависимости: bash, find, date, du (coreutils). Логи: journalctl -u zona-backup -n 50

set -euo pipefail

ZONA_DIR="${ZONA_DIR:-/opt/zona}"
DATA_DIR="${ZONA_DIR}/data"
BACKUP_DIR="${DATA_DIR}/backups"
STAMP="$(date +%Y%m%d)"
OUT="${BACKUP_DIR}/zona-${STAMP}.db"

[ -f "${DATA_DIR}/zona.db" ] || { echo "[zona-backup] нет ${DATA_DIR}/zona.db — нечего копировать"; exit 0; }

mkdir -p "${BACKUP_DIR}"

"${ZONA_DIR}/zn-server" --backup-db "${DATA_DIR}/zona.db" --out "${OUT}"

# Ретеншн 7 суток: устаревшие копии удаляем (свежая сегодняшняя не трогается).
find "${BACKUP_DIR}" -name 'zona-*.db' -type f -mtime +7 -delete

echo "[zona-backup] готово: ${OUT} ($(du -h "${OUT}" | cut -f1))"

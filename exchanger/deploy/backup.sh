#!/usr/bin/env bash
# Ежедневный бэкап базы exch (SPEC §12, шаг 12): pg_dump → шифрование age → копия наружу.
# Приватный ключ age — только у владельца (менеджер паролей), на сервере — публичный.
#   AGE_RECIPIENT=age1…          публичный ключ
#   RCLONE_REMOTE=remote:exch-backups   куда копировать (rclone config); пусто — только локально
#   HC_BACKUP_URL=https://hc-ping.com/…  пинг healthchecks.io (необязательно)
set -euo pipefail
cd /opt/exch
: "${AGE_RECIPIENT:?AGE_RECIPIENT is not set}"
dir=/var/backups/exch
mkdir -p "$dir"
f="$dir/exch-$(date -u +%F-%H%M).dump.age"
docker compose exec -T db pg_dump -U exch -Fc exch | age -r "$AGE_RECIPIENT" > "$f"
test -s "$f"
if [ -n "${RCLONE_REMOTE:-}" ]; then
  rclone copy "$f" "$RCLONE_REMOTE/"
fi
find "$dir" -name 'exch-*.dump.age' -mtime +7 -delete
if [ -n "${HC_BACKUP_URL:-}" ]; then
  curl -fsS -m 10 "$HC_BACKUP_URL" >/dev/null
fi
echo "backup ok: $f"

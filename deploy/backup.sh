#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
resolve_role "${1:-}"
export_compose_ids
backup_root="${BACKUP_DIR:-$APP_DIR/backups}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
archive="$backup_root/${ROLE}-${stamp}.tar.gz"
mkdir -p "$backup_root"
chmod 700 "$backup_root"
listener_was_running=false
if [[ "$ROLE" == listener ]] && compose ps --status running --services | grep -qx listener; then
    listener_was_running=true
    compose stop listener
fi
restore_listener() {
    if [[ "$listener_was_running" == true ]]; then compose start listener; fi
}
trap restore_listener EXIT
compose exec -T redis redis-cli SAVE
paths=(.env .node-role)
if [[ "$ROLE" == listener ]]; then paths+=(sessions); else paths+=(config/api_keys.json); fi
[[ -d "$APP_DIR/logs" ]] && paths+=(logs)
tar -czf "$archive" -C "$APP_DIR" "${paths[@]}"
redis_backup="$backup_root/${ROLE}-${stamp}-redis.rdb"
compose cp redis:/data/dump.rdb "$redis_backup"
chmod 600 "$redis_backup"
chmod 600 "$archive"
restore_listener
trap - EXIT
info "резервная копия создана: $archive и $redis_backup (файлы содержат секреты и сессии; храните их в защищённом месте)"

#!/usr/bin/env bash
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail() { printf 'ОШИБКА: %s\n' "$*" >&2; exit 1; }
info() { printf '[inside-bot] %s\n' "$*"; }

resolve_role() {
    local requested="${1:-${NODE_ROLE:-}}"
    if [[ -z "$requested" && -f "$APP_DIR/.node-role" ]]; then
        requested="$(<"$APP_DIR/.node-role")"
    fi
    if [[ -z "$requested" && -f "$APP_DIR/.env" ]]; then
        if grep -Eq '^[[:space:]]*PYROGRAM_API_ID[[:space:]]*=' "$APP_DIR/.env"; then
            requested=listener
        elif grep -Eq '^[[:space:]]*TRADE_DRY_RUN[[:space:]]*=' "$APP_DIR/.env"; then
            requested=trader
        fi
    fi
    [[ "$requested" == listener || "$requested" == trader ]] || fail "не удалось определить роль узла; запустите deploy/install.sh или укажите listener/trader"
    if [[ -z "${1:-${NODE_ROLE:-}}" && -f "$APP_DIR/.node-role" ]]; then
        requested="$(<"$APP_DIR/.node-role")"
    fi
    ROLE="$requested"
    COMPOSE_FILE="$APP_DIR/compose.$ROLE.yml"
    export ROLE COMPOSE_FILE
}

export_compose_ids() {
    local owner_uid="${SUDO_UID:-$(id -u)}"
    local owner_gid="${SUDO_GID:-$(id -g)}"
    export APP_UID="${APP_UID:-$owner_uid}" APP_GID="${APP_GID:-$owner_gid}"
}

compose() {
    docker compose --project-directory "$APP_DIR" --env-file "$APP_DIR/.env" -f "$COMPOSE_FILE" "$@"
}

require_env_value() {
    local name="$1"
    grep -Eq "^[[:space:]]*${name}[[:space:]]*=[[:space:]]*[^[:space:]#]+" "$APP_DIR/.env" || fail "обязательное значение '$name' в .env не заполнено; отредактируйте $APP_DIR/.env"
}

validate_config() {
    [[ -f "$APP_DIR/.env" ]] || fail "не найден $APP_DIR/.env; скопируйте .env.$ROLE.example и заполните его"
    if [[ "$ROLE" == listener ]]; then
        for name in PYROGRAM_API_ID PYROGRAM_API_HASH PYROGRAM_TARGET_CHAT REDIS_HOST REDIS_PORT REDIS_QUEUE BRIDGE_HOST BRIDGE_PORT; do
            require_env_value "$name"
        done
        return
    fi

    for name in REDIS_HOST REDIS_PORT REDIS_QUEUE REDIS_TICKERS_HASH TRADE_DRY_RUN; do
        require_env_value "$name"
    done
    [[ -f "$APP_DIR/config/api_keys.json" ]] || fail "не найден config/api_keys.json; скопируйте config/api_keys.example.json и добавьте ключи аккаунтов этого узла"
    if grep -Eq 'PUBLIC_API_KEY_HERE|SECRET_API_KEY_HERE' "$APP_DIR/config/api_keys.json"; then
        fail "в config/api_keys.json остались примерные ключи; заполните файл перед запуском торгового узла"
    fi
    [[ -d "$APP_DIR/config" && -d "$APP_DIR/logs" ]] || fail "каталоги конфигурации или журналов торгового узла недоступны"
}

assert_dry_run() {
    [[ "$ROLE" == trader ]] || return 0
    local dry_run
    dry_run="$(sed -nE 's/^[[:space:]]*TRADE_DRY_RUN[[:space:]]*=[[:space:]]*([^[:space:]#]+).*/\1/p' "$APP_DIR/.env" | tail -n 1 | tr '[:upper:]' '[:lower:]')"
    [[ "$dry_run" == true || "$dry_run" == 1 || "$dry_run" == yes || "$dry_run" == on ]] || fail "install.sh запускает торговый узел только при TRADE_DRY_RUN=true; проверьте настройки вручную перед включением реальной торговли"
}

check_legacy_services() {
    local units=()
    if [[ "$ROLE" == listener ]]; then
        units=(telegram_listener)
    else
        units=(rust_trader price_streamer balance_streamer reset_account_config)
    fi
    local conflicts=()
    for unit in "${units[@]}"; do
        if systemctl is-active --quiet "$unit.service" 2>/dev/null; then
            conflicts+=("$unit (active)")
        elif systemctl is-enabled --quiet "$unit.service" 2>/dev/null; then
            conflicts+=("$unit (enabled for boot)")
        fi
    done
    if ((${#conflicts[@]})); then
        fail "службы systemd конфликтуют с Docker: ${conflicts[*]}. Во время запланированного перехода вручную остановите и отключите их; установщик не меняет существующие службы и не использует активные Telegram-сессии"
    fi
}

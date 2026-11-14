#!/usr/bin/env bash
# Первичная установка Docker и развёртывание узла на Ubuntu 22.04/24.04.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

[[ "$(id -u)" -eq 0 ]] || fail "запустите с sudo: sudo bash deploy/install.sh"
[[ -n "${SUDO_UID:-}" && "$SUDO_UID" -ne 0 ]] || fail "запускайте sudo из обычной учётной записи — владельца проекта, чтобы контейнеры использовали её UID/GID"
[[ -r /etc/os-release ]] || fail "не удалось определить операционную систему"
# shellcheck disable=SC1091
source /etc/os-release
[[ "${ID:-}" == ubuntu && ("${VERSION_ID:-}" == 22.04 || "${VERSION_ID:-}" == 24.04) ]] || fail "поддерживаются Ubuntu 22.04 и 24.04"

if [[ -t 0 ]]; then
    read -r -p "Роль узла (listener/trader): " selected_role
else
    selected_role="${NODE_ROLE:-}"
fi
[[ "$selected_role" == listener || "$selected_role" == trader ]] || fail "для запуска без диалога задайте NODE_ROLE=listener или NODE_ROLE=trader"
resolve_role "$selected_role"
if [[ -f "$APP_DIR/.node-role" && "$(<"$APP_DIR/.node-role")" != "$ROLE" ]]; then
    fail "эта копия репозитория уже настроена как $(<"$APP_DIR/.node-role"); для другой роли используйте отдельную копию"
fi

owner_uid="${SUDO_UID:-0}"
owner_gid="${SUDO_GID:-0}"
check_legacy_services

if [[ ! -f "$APP_DIR/.env" ]]; then
    install -o "$owner_uid" -g "$owner_gid" -m 600 "$APP_DIR/.env.$ROLE.example" "$APP_DIR/.env"
    info "файл .env создан из .env.$ROLE.example; заполните его и запустите установщик повторно"
fi
if [[ "$ROLE" == trader && ! -f "$APP_DIR/config/api_keys.json" ]]; then
    install -o "$owner_uid" -g "$owner_gid" -m 600 "$APP_DIR/config/api_keys.example.json" "$APP_DIR/config/api_keys.json"
    info "config/api_keys.json создан из примера; добавьте действительные ключи и запустите установщик повторно"
fi
[[ -d "$APP_DIR/sessions" ]] || install -d -o "$owner_uid" -g "$owner_gid" -m 700 "$APP_DIR/sessions"
[[ -d "$APP_DIR/logs" ]] || install -d -o "$owner_uid" -g "$owner_gid" -m 750 "$APP_DIR/logs"
if [[ ! -f "$APP_DIR/.node-role" ]]; then
    printf '%s\n' "$ROLE" > "$APP_DIR/.node-role"
    chown "$owner_uid:$owner_gid" "$APP_DIR/.node-role"
    chmod 600 "$APP_DIR/.node-role"
fi
validate_config
assert_dry_run

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y ca-certificates curl gnupg
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor --yes -o /etc/apt/keyrings/docker.gpg
chmod a+r /etc/apt/keyrings/docker.gpg
arch="$(dpkg --print-architecture)"
codename="${UBUNTU_CODENAME:-$VERSION_CODENAME}"
printf 'deb [arch=%s signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu %s stable\n' "$arch" "$codename" > /etc/apt/sources.list.d/docker.list
apt-get update
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl enable --now docker
docker info >/dev/null
docker compose version

export_compose_ids
compose build
if [[ "$ROLE" == trader ]]; then
    compose run --rm --no-deps price-streamer python deploy/validate_api_keys.py config/api_keys.json
fi
compose config --quiet

if [[ "$ROLE" == listener ]] && ! compgen -G "$APP_DIR/sessions/listener_*.session" >/dev/null; then
    info "Telegram-сессия не найдена. Запускается интерактивное создание сессии через generate_session.py."
    compose run --rm --no-deps listener python generate_session.py
fi

validate_config
compose up -d

for attempt in $(seq 1 30); do
    if [[ "$ROLE" == listener ]]; then
        if compose ps --status running --services | grep -qx listener && compose exec -T redis redis-cli ping | grep -qx PONG; then break; fi
    else
        if compose ps --status running --services | grep -qx rust-trader && compose exec -T redis redis-cli ping | grep -qx PONG; then break; fi
    fi
    [[ "$attempt" -lt 30 ]] || fail "контейнеры не перешли в состояние запуска; проверьте журналы командой deploy/logs.sh $ROLE"
    sleep 2
done
compose ps
info "установка роли $ROLE завершена. Для торгового узла требовалось TRADE_DRY_RUN=true."

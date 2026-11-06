#!/usr/bin/env bash
# ============================================================
# Rocket v2 — Ubuntu provisioning for the Trading Node (Tokyo)
#
# Устанавливает системные пакеты, Rust toolchain, собирает Rust-движок,
# поднимает Python-окружение для стримеров и ставит systemd-юниты.
#
# Запускать:  sudo ./deploy/install.sh
# Переменные: RUN_USER=<пользователь>  (по умолчанию root)
# ============================================================
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_USER="${RUN_USER:-root}"
VENV="$APP_DIR/venv"
BIN="$APP_DIR/engine_rust/target/release/rust_trader"

log()  { printf '\033[1;32m[rocket]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[rocket]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[rocket]\033[0m %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -eq 0 ]] || die "run as root: sudo ./deploy/install.sh"
[[ -f "$APP_DIR/config/api_keys.json" ]] || warn "missing config/api_keys.json - cp config/api_keys.example.json config/api_keys.json and fill it in"
[[ -f "$APP_DIR/.env" ]] || warn "missing .env - copy deploy/.env.example to .env and edit it"

# ---- 1. system packages ------------------------------------------------
log "Installing system packages..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y --no-install-recommends \
    build-essential pkg-config curl ca-certificates git \
    python3 python3-venv python3-dev python3-pip \
    redis-server libssl-dev libffi-dev

# ---- 2. Rust toolchain -------------------------------------------------
if command -v cargo >/dev/null 2>&1; then
    log "Rust already installed: $(cargo --version)"
else
    log "Installing Rust (rustup, minimal profile)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

# ---- 3. build the engine ------------------------------------------------
log "Building Rust engine (release, locked)..."
( cd "$APP_DIR/engine_rust" && cargo build --release --locked )
[[ -x "$BIN" ]] || die "binary not found at $BIN"

log "Running offline self-test..."
"$BIN" --selftest || die "self-test failed"

# ---- 4. Python venv for streamers ---------------------------------------
log "Setting up Python venv..."
if [[ ! -x "$VENV/bin/python3" ]]; then
    python3 -m venv "$VENV"
fi
"$VENV/bin/pip" install --upgrade pip wheel >/dev/null
"$VENV/bin/pip" install -r "$APP_DIR/requirements_trader.txt"

# ---- 5. systemd units ----------------------------------------------------
log "Installing systemd units (run user: $RUN_USER)..."
for unit in rust_trader price_streamer balance_streamer reset_account_config; do
    sed \
        -e "s|__APP_DIR__|$APP_DIR|g" \
        -e "s|__RUN_USER__|$RUN_USER|g" \
        "$APP_DIR/deploy/$unit.service" > "/etc/systemd/system/$unit.service"
    log "  -> /etc/systemd/system/$unit.service"
done

systemctl daemon-reload
systemctl enable --now redis-server
systemctl enable rust_trader price_streamer balance_streamer reset_account_config

# ---- 6. ownership (если юниты работают не от root) ----------------------
if [[ "$RUN_USER" != "root" ]]; then
    chown -R "$RUN_USER":"$RUN_USER" "$APP_DIR"
    log "project ownership handed to $RUN_USER"
fi

cat <<'EOF'

============================================================
Готово. Осталось настроить и запустить:

  1. config/api_keys.json  — ключи аккаунтов (founders/friends/clients)
  2. .env                  — cp deploy/.env.example .env  (TRADE_DRY_RUN=true для первого старта)
  3. systemctl start price_streamer balance_streamer
     sleep 15
     systemctl start reset_account_config rust_trader

Проверка:
  systemctl status rust_trader
  journalctl -u rust_trader -f
  journalctl -u price_streamer -f

Когда dry-run пройден — поставьте TRADE_DRY_RUN=false, перезапустите движок:
  systemctl restart rust_trader
============================================================
EOF

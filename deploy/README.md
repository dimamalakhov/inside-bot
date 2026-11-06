# Развёртывание на Ubuntu — Trading Node (Токио)

Этот каталог готовит Ubuntu-сервер, который исполняет сделки: Redis, два
Python-стримера (цены и балансы), настройку плеч и Rust-движок. Всё управляется
systemd и переживает перезагрузку.

Полная инструкция по обоим узлам (включая перенос кода на сервер, Европу и
firewall) — в основном [`README.md`](../README.md), раздел
«📦 Полный деплой на сервер». Здесь — детали именно этого каталога.

## Что где

| Файл | Назначение |
|---|---|
| `install.sh` | идемпотентная установка: пакеты, Rust, сборка, venv, systemd |
| `rust_trader.service` | Rust-движок (читает `trade_queue` + UDP :5005, шлёт ордера) |
| `price_streamer.service` | цены Binance -> Redis (`futures_tickers`) |
| `balance_streamer.service` | балансы Binance -> Redis (`balance:<owner>`) |
| `reset_account_config.service` | плечо + `ISOLATED`-маржа (цикл 6 часов) |
| `telegram_listener.service` | слушатель Telegram (для узла-слушателя) |
| `.env.example` | шаблон `.env` торгового узла |
| `.env.listener.example` | шаблон `.env` узла-слушателя |

## Требования

- Ubuntu 20.04 / 22.04 / 24.04 (amd64), root или sudo.
- Исходящие: TCP 443 → `fapi.binance.com`, `ws-fapi.binance.com`; TCP 9443 →
  `fstream.binance.com`.
- Входящие: UDP 5005 с IP Европы (Bridge), SSH с вашего IP.

## Установка

```bash
# 1. Код на сервер (публичный репозиторий)
sudo apt update && sudo apt install -y git
sudo mkdir -p /opt/rocket && sudo chown "$USER":"$USER" /opt/rocket
git clone https://github.com/dimamalakhov/inside-bot.git /opt/rocket

#    Если репозиторий приватный — см. «Вариант A» в основном README
#    (SSH-ключ или токен). Без git: архив с GitHub или rsync/scp.

# 2. Конфигурация
cp /opt/rocket/deploy/.env.example /opt/rocket/.env
nano /opt/rocket/.env                         # TRADE_DRY_RUN=true для первого запуска
cp /opt/rocket/config/api_keys.example.json /opt/rocket/config/api_keys.json
nano /opt/rocket/config/api_keys.json         # founders/friends/clients (+ support для цен)

# 3. Firewall (ufw)
sudo ufw allow OpenSSH
sudo ufw allow from <IP_ЕВРОПЫ> to any port 5005 proto udp
sudo ufw enable

# 4. Установка и сборка (идемпотентно, можно перезапускать)
cd /opt/rocket && sudo bash deploy/install.sh

# 5. Запуск (порядок важен: сначала цены/балансы, потом плечи и движок)
sudo systemctl start price_streamer balance_streamer
sleep 20                                      # прогрев цен и балансов
sudo systemctl start reset_account_config
sudo systemctl start rust_trader
```

## Проверка

```bash
systemctl status rust_trader price_streamer balance_streamer reset_account_config redis-server

# логи движка (метрики heartbeat раз в минуту)
journalctl -u rust_trader -f
journalctl -u price_streamer -f

# офлайн-самопроверка в любой момент
/opt/rocket/engine_rust/target/release/rust_trader --selftest

# что видит движок в Redis
redis-cli hlen futures_tickers
redis-cli get balance:<owner>
```

Первый старт в `TRADE_DRY_RUN=true`: движок считает размер позиции и TP/SL,
логирует ордера, но ничего не отправляет на биржу. Убедитесь, что в логе
появляются строки `[dry-run] market order suppressed` с корректными объёмами,
затем:

```bash
sudo sed -i 's/^TRADE_DRY_RUN=.*/TRADE_DRY_RUN=false/' /opt/rocket/.env
sudo systemctl restart rust_trader
```

## Обновление

```bash
cd /opt/rocket && git pull          # или замените каталог новой версией
sudo bash deploy/install.sh         # пересоберёт и обновит юниты
sudo systemctl restart rust_trader price_streamer balance_streamer reset_account_config
```

## Удаление / откат

```bash
sudo systemctl disable --now rust_trader price_streamer balance_streamer reset_account_config
sudo rm -f /etc/systemd/system/{rust_trader,price_streamer,balance_streamer,reset_account_config}.service
sudo systemctl daemon-reload
sudo rm -rf /opt/rocket             # при необходимости
```

## Заметки

- Сборка выполняется с `--locked`, т.е. по зафиксированному `Cargo.lock` —
  результат воспроизводим между машинами.
- Если юниты должны работать не от root — `RUN_USER=trading sudo bash deploy/install.sh`.
- Бинарник на Linux называется `rust_trader` (без `.exe`), `launcher.py` сам
  выбирает нужное имя по ОС.
- `reset_account_config` стартует только после наполнения `futures_tickers`:
  иначе он пишет ошибку и ждёт следующего цикла (6 часов).
- Listener Node (Европа) — чисто Python, без Rust: `telegram_listener.service`
  и `.env.listener.example` из этого каталога, пошагово — в основном README.

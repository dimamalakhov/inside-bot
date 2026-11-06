# Развёртывание на Ubuntu — Trading Node (Токио)

Этот каталог готовит Ubuntu-сервер, который исполняет сделки: Redis, два
Python-стримера (цены и балансы) и Rust-движок. Всё управляется systemd и
переживает перезагрузку.

## Что где

| Файл | Назначение |
|---|---|
| `install.sh` | идемпотентная установка: пакеты, Rust, сборка, venv, systemd |
| `rust_trader.service` | Rust-движок (читает `trade_queue` + UDP :5005, шлёт ордера) |
| `price_streamer.service` | цены Binance -> Redis (`futures_tickers`) |
| `balance_streamer.service` | балансы Binance -> Redis (`balance:<owner>`) |
| `.env.example` | шаблон `.env` для торгового узла |

## Требования

- Ubuntu 20.04 / 22.04 / 24.04 (amd64), root или sudo.
- Исходящие: TCP 443 → `fapi.binance.com`, `ws-fapi.binance.com`; TCP 9443 →
  `fstream.binance.com`.
- Входящие: UDP 5005 с IP Европы (Bridge), SSH с вашего IP.

## Установка

```bash
# 1. Положите проект (например, в /opt/rocket; путь без пробелов)
sudo mkdir -p /opt/rocket
#    scp/git-clone сюда: launcher.py, engine_rust/, price_streamer.py,
#    balance_streamer.py, utils/, config/, requirements_trader.txt, deploy/

# 2. Конфигурация
sudo cp /opt/rocket/deploy/.env.example /opt/rocket/.env
sudo nano /opt/rocket/.env                    # TRADE_DRY_RUN=true для первого запуска
sudo cp /opt/rocket/config/api_keys.example.json /opt/rocket/config/api_keys.json
sudo nano /opt/rocket/config/api_keys.json    # founders/friends/clients (+ support для цен)

# 3. Firewall (ufw)
sudo ufw allow OpenSSH
sudo ufw allow from <IP_ЕВРОПЫ> to any port 5005 proto udp
sudo ufw enable

# 4. Установка и сборка (идемпотентно, можно перезапускать)
cd /opt/rocket && sudo bash deploy/install.sh

# 5. Запуск
sudo systemctl start price_streamer balance_streamer
sleep 5                                       # прогрев цен/балансов
sudo systemctl start rust_trader
```

## Проверка

```bash
systemctl status rust_trader price_streamer balance_streamer redis-server

# логи движка (метрики heartbeat раз в минуту)
journalctl -u rust_trader -f
journalctl -u price_streamer -f

# офлайн-самопроверка в любой момент
/opt/rocket/engine_rust/target/release/rust_trader --selftest
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
cd /opt/rocket && git pull          # или замените engine_rust/ новой версией
sudo bash deploy/install.sh         # пересоберёт и обновит юниты
sudo systemctl restart rust_trader price_streamer balance_streamer
```

## Удаление / откат

```bash
sudo systemctl disable --now rust_trader price_streamer balance_streamer
sudo rm -f /etc/systemd/system/{rust_trader,price_streamer,balance_streamer}.service
sudo systemctl daemon-reload
sudo rm -rf /opt/rocket             # при необходимости
```

## Заметки

- Сборка выполняется с `--locked`, т.е. по зафиксированному `Cargo.lock` —
  результат воспроизводим между машинами.
- Если юниты должны работать не от root — `RUN_USER=trading sudo bash deploy/install.sh`.
- Бинарник на Linux называется `rust_trader` (без `.exe`), `launcher.py` сам
  выбирает нужное имя по ОС.
- Listener Node (Европа) разворачивается по основному `README.md` — это чисто
  Python (`telegram_listener.py`, `bridge_client.py`, Redis) без Rust.

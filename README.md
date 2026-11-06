# 🚀 Rocket v2: HFT News Trading Bot

Высокопроизводительный гибридный (Python/Rust) торговый бот для Binance Futures.

## 📂 Структура Проекта

```text
.
├── launcher.py              # Точка входа (режимы rust / python / duel)
├── engine_python.py         # Торговый движок Python (AsyncIO + Local Cache)
├── engine_rust/             # Торговый движок Rust (WS API, <1ms hot path)
│   ├── Cargo.toml
│   ├── Cargo.lock
│   └── src/
│       ├── main.rs            # запуск, graceful shutdown, heartbeat
│       ├── config.rs          # переменные окружения (.env)
│       ├── signal.rs          # парсер сигналов (Telegram-текст + JSON)
│       ├── cache.rs           # кэш цен/балансов из Redis
│       ├── dedup.rs           # локальная дедупликация сигналов
│       ├── exchange_info.rs   # фильтры символов (step/tick/minNotional)
│       ├── math.rs            # fixed-point арифметика (u64/u128, 1e8)
│       ├── binance_ws.rs      # WebSocket API: order.place + TP/SL
│       ├── executor.rs        # hot path: сигнал -> ордера по всем аккаунтам
│       ├── bridge.rs          # UDP-приём сигналов из Европы
│       ├── queue.rs           # BLPOP trade_queue с переподключением
│       ├── selftest.rs        # офлайн-проверка (--selftest)
│       └── metrics.rs         # счётчики и латентность
├── deploy/                  # Развёртывание на Ubuntu (systemd + install.sh)
│   ├── install.sh              # установка пакетов, Rust, сборка, systemd
│   ├── rust_trader.service     # юнит движка
│   ├── price_streamer.service  # юнит стримера цен
│   ├── balance_streamer.service# юнит стримера балансов
│   ├── .env.example            # шаблон .env торгового узла
│   └── README.md               # Ubuntu runbook
├── telegram_listener.py     # Слушатель сигналов Telegram (Pyrogram)
├── bridge_client.py         # Отправка сигналов по UDP (Европа -> Токио)
├── price_streamer.py        # Стример цен (WebSockets -> Redis)
├── balance_streamer.py      # Монитор балансов (WebSockets -> Redis)
├── reset_account_config.py  # Плечо + ISOLATED-режим по аккаунтам (периодически)
├── generate_session.py      # Генерация Telegram-сессии (Listener Node)
├── config/
│   ├── api_keys.json        # ключи аккаунтов (локально, в git не коммитится)
│   └── api_keys.example.json# шаблон для нового деплоя
├── sessions/                # *.session — сессии Telegram (Listener Node)
├── utils/
│   └── logger.py            # неблокирующий логгер (queue + handlers)
├── requirements_listener.txt
├── requirements_trader.txt
├── .env                     # локальная конфигурация (не коммитить)
└── .gitignore
```

## 🌍 Архитектура

Гео-сплит: **Европа (Listener Node)** рядом с Telegram DC2 и **Токио (Trading Node)** рядом с Binance.
Используйте раздельные Python-зависимости: `requirements_listener.txt` для слушателя и `requirements_trader.txt` для торгового узла.

### Сервер 1: Европа (Listener Node)

**Что установить и подготовить:**
- Базовые пакеты (сначала):
  ```bash
  sudo apt update && sudo apt upgrade -y
  sudo apt install -y build-essential python3-dev
  sudo apt install -y python3 python3-pip python3-venv python-dotenv
  sudo apt install -y redis-server
  ```
- Python + venv: `python3 -m venv venv && source venv/bin/activate && pip install -r requirements_listener.txt`  
- Redis локально (для дедупликации сигналов).
- Файлы: `telegram_listener.py`, `bridge_client.py`, `generate_session.py` (для генерации сессий), `utils/logger.py`, `.env`, `sessions/`.

**Конфигурация `.env`:**
```ini
PYROGRAM_API_ID=...
PYROGRAM_API_HASH=...
PYROGRAM_TARGET_CHAT=...
REDIS_HOST=127.0.0.1
REDIS_PORT=6379
REDIS_QUEUE=trade_queue
BRIDGE_HOST=<IP_TOKYO_SERVER>
BRIDGE_PORT=5005
```

**Сеть / Firewall:**
- Outbound: UDP 5005 → IP Токио; TCP 443 → api.telegram.org.
- Inbound: SSH (22) с вашего IP; остальное закрыто.

### Сервер 2: Токио (Trading Node)

**Что установить и подготовить:**
- Базовые пакеты (сначала):
  ```bash
  sudo apt update && sudo apt upgrade -y
  sudo apt install -y build-essential python3-dev
  sudo apt install -y python3 python3-pip python3-venv
  sudo apt install -y redis-server
  ```
- Python + venv: `python3 -m venv venv && source venv/bin/activate && pip install -r requirements_trader.txt`  
  (отдельный requirements для Trading: бинансовые SDK/WebSocket, asyncio-стек, Redis-клиент и пр.)
- Rust toolchain и сборка (рекомендуемый путь — `sudo bash deploy/install.sh`, см. `deploy/README.md`):
  ```bash
  cd engine_rust
  cargo build --release --locked   # бинарник: engine_rust/target/release/rust_trader (Linux, без .exe)
  ./target/release/rust_trader --selftest   # офлайн-проверка сборки, без сети
  cd ..
  ```
- Redis локально.
- Файлы: `launcher.py`, `engine_python.py` (опционально), `engine_rust/` (собранный), `price_streamer.py`, `balance_streamer.py`, `reset_account_config.py` (плечо/режим маржи), `utils/logger.py`, `config/api_keys.json`, `.env`.

**Конфигурация `.env`:**
```ini
# API-ключи аккаунтов — в config/api_keys.json (не в .env)
REDIS_HOST=127.0.0.1
REDIS_PORT=6379
BRIDGE_PORT=5005
TRADE_DRY_RUN=true          # первый запуск — dry-run, затем false
# полный список параметров — в deploy/.env.example и разделе ниже
```

**Сеть / Firewall:**
- Inbound: UDP 5005 ← IP Европы; SSH (22) с вашего IP.
- Outbound: TCP 443 → fapi.binance.com; TCP 443 → ws-fapi.binance.com; TCP 9443 → fstream.binance.com.

**Схема работы:**
```
[Telegram DC2] -> [Listener EU] -> [UDP Bridge] -> [Trader Tokyo] -> [Binance]
     ~10ms           ~1ms             ~80ms            ~1ms           ~5ms
```

## 🦀 Rust-движок: как он устроен

### Путь сигнала (hot path)

```
trade_queue (BLPOP) ─┐
                     ├─> parse (текст Telegram | JSON)  ~1 µs
UDP :5005 ───────────┘            │
                                  ├─> локальный dedup (Fnv64, 0 RTT)
                                  ├─> Redis-lock processed_signal:* (NX/EX)
                                  ├─> тикер из памяти (или HGET при простое)
                                  ├─> фильтры из exchangeInfo (в памяти)
                                  └─> на каждый аккаунт параллельно:
                                        MARKET order.place (WS API, RESULT)
                                        └─> TP + SL (две подписи, параллельно)
```

- **ORDER через WebSocket API** (`wss://ws-fapi.binance.com/ws-fapi/v1`), а не REST:
  соединение уже прогрето (TLS/HTTP2 handshake не платится на каждый ордер),
  один кадр в обе стороны.
- **`newOrderRespType=RESULT`** — Binance возвращает `avgPrice`/`executedQty`
  прямо в ответе на `order.place`, поэтому TP/SL ставятся сразу, без поллинга
  `order.status` (в старой версии это стоило до 2 секунд). Поллинг остался
  только как резерв, если биржа не вернула цену исполнения.
- **Fixed-point арифметика**: цены и объёмы в `u64` с масштабом `1e8`,
  умножения через `u128`. Никаких `f64`-округлений в расчёте количества,
  никаких переполнений (старая версия переполняла `u64` на реальных балансах).
- **Нулевые аллокации в горячем пути**: тикер — `Copy`, сторона — `enum Side`,
  дедупликация по 64-битному хешу (ключ не собирается в `String`),
  баланс — атомарное чтение из массива по индексу аккаунта.
- **TP/SL**: `TAKE_PROFIT` и `STOP` (reduceOnly, GTC) — те же типы ордеров, что
  ставит Python-движок; округление «в пользу позиции»: TP от цены входа, SL
  к триггеру, лимит стопа с запасом `SL_LIMIT_SLIPPAGE_*`.
- **Устойчивость**: WS-сессия переподключается с экспоненциальным backoff,
  все «висящие» запросы получают ошибку при разрыве (запрос никогда не виснет),
  Redis-подключения пересоздаются, BLPOP не блокирует мультиплексированное
  соединение.
- **Наблюдаемость**: раз в минуту `heartbeat` печатает счётчики — сколько
  сигналов, дублей, отказов биржи, среднюю/максимальную задержку
  `сигнал -> подтверждение ордера` в микросекундах.

### Три уровня дедупликации

| Уровень | Ключ | Зачем |
|---|---|---|
| UDP/память | FNV64(symbol+side), TTL 60с | 3 копии UDP-пакета, правки сообщения в Telegram |
| Redis | `processed_signal:<SYMBOL>_<SIDE>` NX EX 60 | два процесса (duel mode), рестарт движка |
| Снятие противоположного лока | `processed_signal:<SYMBOL>_<OPPOSITE>` DEL | реальный разворот не блокируется |

Ключ Redis совпадает с тем, что использует `engine_python.py`, поэтому
`--mode duel` не приводит к двойным сделкам.

### Что было исправлено в существующем коде

| Проблема | Последствие | Исправление |
|---|---|---|
| `binance_ws.rs` не компилировался: `cmd_rx` перемещался в задачу внутри цикла реконнекта, `ws_sink.clone()` (SplitSink не Clone), `response_tx` использовался после перемещения | сборка невозможна | WS-клиент переписан: один владелец сокета, `select!` вместо вложенных задач |
| В подписанный запрос не добавлялся `apiKey` | Binance отклонил бы любой `order.place` | `apiKey` включён в `params` и в подпись |
| Балансы читались из хеша `futures_balances`, который никто не пишет (`balance_streamer.py` пишет `balance:<owner>`) | движок всегда видел 0 → не торговал | `MGET balance:*` + fallback на хеш |
| Сигнал парсился только как JSON, а `telegram_listener.py` кладёт сырой текст Telegram | ни один сигнал не распознавался | парсятся оба формата (`#BTC Bullish` и `{"symbol","side"}`) |
| `Config` требовал `BINANCE_API_KEY`/`BINANCE_SECRET_KEY` из `.env`, которых там нет | падение на старте | ключи берутся из `config/api_keys.json`, `.env` не обязателен |
| `TRADE_ALLOCATION=0.3`, `TP_PERCENT=0.3` смешивались с `*_BPS` | неверные объёмы/TP/SL | поддержаны обе формы записи |
| `calculate_quantity`: `notional * 1e8` переполнял `u64` | паника в debug, тихо неверный объём в release | `u128` + saturating |
| TP/SL ставились поллингом до 2с | позиция без стопа до 2 секунд | `RESULT`-ответ, поллинг только как fallback |
| `format!("{:x}", order_id)[..8]` | паника на коротком hex | идентификатор больше не режется |
| Дубли аккаунтов по `owner` в `api_keys.json` | два клиента на один аккаунт | дедупликация по `owner` |
| Нет graceful shutdown, нет метрик | «слепой» процесс | `ctrl-c`, heartbeat, счётчики |

## ⚙️ Переменные окружения Rust-движка

Все необязательны; значения по умолчанию — продовые.

| Переменная | По умолчанию | Назначение |
|---|---|---|
| `REDIS_HOST` / `REDIS_PORT` / `REDIS_DB` | `127.0.0.1` / `6379` / `0` | адрес Redis |
| `REDIS_URL` | — | полный URL, перекрывает host/port/db |
| `REDIS_QUEUE` | `trade_queue` | очередь сигналов (совместимо с Python) |
| `REDIS_TICKERS_HASH` | `futures_tickers` | хеш тикеров от `price_streamer.py` |
| `BALANCE_KEY_PREFIX` | `balance:` | ключи балансов от `balance_streamer.py` |
| `REDIS_BALANCES_HASH` | `futures_balances` | legacy-хеш балансов (fallback) |
| `TRADE_ALLOCATION` | `0.3` | доля баланса в сделку (30%) |
| `TRADE_ALLOCATION_BPS` | — | то же в базисных пунктах, перекрывает |
| `TP_PERCENT` / `TP_PERCENT_BPS` | `0.3` / — | тейк-профит |
| `SL_PERCENT` / `SL_PERCENT_BPS` | `0.3` / — | стоп-лосс |
| `SL_LIMIT_SLIPPAGE_PERCENT` / `_BPS` | `0.5` / — | запас лимитной цены стопа |
| `BINANCE_TESTNET` | `false` | testnet-эндпоинты |
| `BINANCE_WS_API_URL` / `BINANCE_REST_URL` | авто | переопределение эндпоинтов |
| `API_KEYS_FILE` | `config/api_keys.json` | файл с ключами аккаунтов |
| `TRADE_DRY_RUN` | `false` | считать и логировать, но не отправлять ордера |
| `BRIDGE_PORT` | `5005` | UDP-приём сигналов из Европы |
| `BRIDGE_FORWARD_REDIS` | `false` | дублировать UDP-сигналы в `trade_queue` |
| `DUPLICATE_SIGNAL_LOCK_SEC` | `60` | окно антидубля |
| `REDIS_DEDUP` | `true` | использовать Redis-лок `processed_signal:*` |
| `TICKER_UPDATE_INTERVAL_MS` | `100` | частота обновления кэша цен |
| `BALANCE_UPDATE_INTERVAL_MS` | `500` | частота обновления балансов |
| `MAX_TICKER_AGE_MS` | `2000` | после этого цена дозапрашивается из Redis |
| `ORDER_TIMEOUT_MS` | `3000` | таймаут ответа на `order.place` |
| `RECV_WINDOW_MS` | `5000` | `recvWindow` подписи |
| `FILL_POLL_ATTEMPTS` / `FILL_POLL_INTERVAL_MS` | `10` / `150` | резервный поллинг исполнения |
| `RUST_LOG` | `info` | уровень логов (`RUST_LOG=debug`) |

## 🎮 Использование

### Сервер 1 (Listener EU)
```bash
# 1) Запустить Redis (локально)
redis-server

# 2) Запустить слушателя, отправляющего в Bridge
python3 telegram_listener.py
# при необходимости раскомментировать вызов bridge_client в коде
```

### Сервер 2 (Trader Tokyo)
```bash
# 1) Запустить Redis
redis-server

# 2) Запустить стримеры
python3 price_streamer.py &
python3 balance_streamer.py &

# 3) Запустить движок
python3 launcher.py --mode rust   # минимальная задержка
# альтернативы:
# python3 launcher.py --mode python
# python3 launcher.py --mode duel
```

На Ubuntu рекомендуется управление через systemd (`sudo bash deploy/install.sh`):
юниты `rust_trader`, `price_streamer`, `balance_streamer` перезапускаются
автоматически и стартуют при загрузке. Подробности — в `deploy/README.md`.

### Проверка и отладка Rust-движка

```bash
cd engine_rust

cargo test                 # 40 модульных тестов (парсинг, фильтры, round-trip ордера)
./target/release/rust_trader --selftest   # офлайн-прогон решения по синтетическим данным
./target/release/rust_trader --help       # список переменных окружения

TRADE_DRY_RUN=1 RUST_LOG=debug ./target/release/rust_trader
# ^ боевой цикл без отправки ордеров: видно размер позиции, TP/SL и задержки
```

Пример вывода `--selftest`:
```
  signal ok  : "#BTC Bullish setup, entry 62k" -> BTCUSDT BUY
  filters    : step=100000 tick=10000000 minNotional=500000000 qtyDec=3 priceDec=1
  sizing     : balance=1000 USDT alloc=30% lev=10x -> notional=3000 USDT qty=0.06
  bracket Buy: entry=50000 tp=50150 sl=49850 sl_limit=49600.7 qty=0.06
```

## 🧰 Служебные скрипты

Два скрипта не входят в hot path, но нужны для корректной работы:

| Скрипт | Нода | Зачем |
|---|---|---|
| `reset_account_config.py` | Trading (Токио) | каждые 6 часов выставляет плечо и `ISOLATED`-режим маржи по всем аккаунтам/символам. Движок считает объём по `ticker.leverage`, но само плечо на бирже не меняет — этот скрипт делает настройки заранее |
| `generate_session.py` | Listener (Европа) | создаёт новую Telegram-сессию (`listener_1.session`) при первом подключении нового аккаунта Pyrogram |

Запуск `reset_account_config.py` — на Trading Node после того, как `price_streamer.py`
уже наполнил `futures_tickers`:

```bash
python3 reset_account_config.py   # работает фоном с циклом 6 часов
```

## ⚠️ Важные замечания

- Ключи API читаются только из `config/api_keys.json` (группы `founders`,
  `friends`, `clients`; `support` — только для стримеров цен). Для нового
  окружения: `cp config/api_keys.example.json config/api_keys.json` и заполнить.
- Балансы берутся из `balance:<owner>` — их пишет `balance_streamer.py`.
  Пока баланса нет, аккаунт пропускается (в логе `zero balance`).
- TP/SL ставятся с `reduceOnly=true`, то есть корректны в **one-way mode**.
  Если на аккаунте включён **hedge mode**, нужен `positionSide` — это
  единственное место, где движок потребует доработки под ваш режим.
- `engine_rust/target/` и `venv/` — локальные артефакты сборки, создаются на
  сервере при `deploy/install.sh`, в git не нужны (см. `.gitignore`).

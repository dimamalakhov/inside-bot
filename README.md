# 🚀 Rocket v2: HFT News Trading Bot

Высокопроизводительный гибридный (Python/Rust) торговый бот для Binance Futures:
сигналы из Telegram → мгновенное исполнение ордеров по всем аккаунтам + TP/SL.

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
├── deploy/                  # Развёртывание на Ubuntu
│   ├── install.sh              # пакеты, Rust, сборка, venv, systemd (Trading Node)
│   ├── rust_trader.service     # юнит движка
│   ├── price_streamer.service  # юнит стримера цен
│   ├── balance_streamer.service# юнит стримера балансов
│   ├── reset_account_config.service
│   ├── telegram_listener.service   # юнит слушателя (Listener Node)
│   ├── .env.example            # шаблон .env торгового узла
│   ├── .env.listener.example   # шаблон .env узла-слушателя
│   └── README.md               # детали systemd, обновление, откат
├── telegram_listener.py     # Слушатель сигналов Telegram (Pyrogram)
├── bridge_client.py         # Ручная отправка сигнала по UDP (Европа -> Токио)
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

## 🌍 Архитектура и топологии

| Топология | Когда | Что дополнительно нужно |
|---|---|---|
| **Одна машина** | быстрый старт, тесты, небольшие объёмы | ничего |
| **Гео-сплит** (Европа + Токио) | продакшн с минимальной задержкой | UDP-мост EU → Tokyo |

Гео-сплит:

```
[Telegram DC2] -> [Listener EU] -> [UDP Bridge] -> [Trader Tokyo] -> [Binance]
     ~10ms           ~1ms             ~80ms            ~1ms           ~5ms
```

Что где работает:

| Узел | Процессы | Зависимости |
|---|---|---|
| Европа | `redis-server`, `telegram_listener` | `requirements_listener.txt` |
| Токио | `redis-server`, `price_streamer`, `balance_streamer`, `reset_account_config`, `rust_trader` | `requirements_trader.txt` + Rust |

Порты:

| Где | Порт / протокол | Назначение |
|---|---|---|
| EU → Tokyo | UDP 5005 | сигналы (мост) |
| EU | TCP 443 | api.telegram.org |
| Tokyo | TCP 443 | fapi.binance.com, ws-fapi.binance.com |
| Tokyo | TCP 9443 | fstream.binance.com |
| обе | TCP 22 | SSH (только ваш IP) |
| обе | TCP 6379 | Redis (только 127.0.0.1) |

## 📦 Полный деплой на сервер

### 0. Что понадобится

- Ubuntu 20.04 / 22.04 / 24.04 (amd64), пользователь с `sudo`.
- Доступ к репозиторию `github.com/dimamalakhov/inside-bot`.
- **Для Токио:** API-ключи Binance (группы `founders`/`friends`/`clients`, плюс один
  `support` — с него `price_streamer` читает цены и плечи).
- **Для Европы:** `api_id` / `api_hash` с [my.telegram.org](https://my.telegram.org)
  (API development tools) и ID целевого чата.


Перенос готовых Telegram-сессий (если они уже есть на локальной машине):

```bash
scp sessions/listener_*.session user@SERVER:/opt/rocket/sessions/
```

**Чего в репозитории нет** (создаётся на сервере, потому что это секреты):

| Файл | Как создать |
|---|---|
| `.env` (Токио) | `cp deploy/.env.example .env` |
| `.env` (Европа) | `cp deploy/.env.listener.example .env` |
| `config/api_keys.json` | `cp config/api_keys.example.json config/api_keys.json` |
| `sessions/*.session` | `venv/bin/python3 generate_session.py` на сервере либо `scp` |

### 3. Топология «гео-сплит» (Европа + Токио)

#### 3.1 Европа — Listener Node

Rust здесь не нужен: только Python, Redis и сессии Telegram.

```bash
# 1) код
git clone https://github.com/dimamalakhov/inside-bot.git /opt/rocket && cd /opt/rocket

# 2) пакеты и окружение
sudo apt update
sudo apt install -y python3 python3-venv redis-server
python3 -m venv venv
venv/bin/pip install -r requirements_listener.txt

# 3) конфигурация
cp deploy/.env.listener.example .env
nano .env            # PYROGRAM_API_ID / API_HASH / TARGET_CHAT,
                     # BRIDGE_HOST=<публичный IP Токио>, BRIDGE_PORT=5005

# 4) Redis
sudo systemctl enable --now redis-server

# 5) сессии Telegram: либо сгенерировать на месте...
venv/bin/python3 generate_session.py
# ...либо перенести готовые (тогда шаг 5 пропускается):
#   scp sessions/listener_*.session user@EU:/opt/rocket/sessions/

# 6) systemd-юнит
sed -e "s|__APP_DIR__|/opt/rocket|g" -e "s|__RUN_USER__|$USER|g" \
  deploy/telegram_listener.service | sudo tee /etc/systemd/system/telegram_listener.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now telegram_listener

# 7) firewall
sudo ufw allow OpenSSH
sudo ufw enable
```

Если `BRIDGE_HOST` задан, каждый сигнал уходит в Токио по UDP (3 копии подряд, как
в `bridge_client.py`). Без него сигналы останутся только в локальном Redis.

#### 3.2 Токио — Trading Node

```bash
# 1) код
git clone https://github.com/dimamalakhov/inside-bot.git /opt/rocket && cd /opt/rocket

# 2) конфиги
cp deploy/.env.example .env
cp config/api_keys.example.json config/api_keys.json
nano .env                     # REDIS_*, TRADE_DRY_RUN=true, TP/SL
nano config/api_keys.json     # founders / friends / clients / support

# 3) установка: пакеты, Rust, сборка, venv, systemd
sudo bash deploy/install.sh

# 4) запуск (порядок важен)
sudo systemctl enable --now redis-server price_streamer balance_streamer
sleep 20                                   # ждём futures_tickers и балансы
sudo systemctl start reset_account_config
sudo systemctl start rust_trader

# 5) firewall
sudo ufw allow OpenSSH
sudo ufw allow from <IP_ЕВРОПЫ> to any port 5005 proto udp
sudo ufw enable
```

#### 3.3 Проверка связки

```bash
# на Токио: движок слушает UDP и готов
sudo journalctl -u rust_trader -n 50 --no-pager
#   ожидаем "UDP bridge listening" и "engine fully operational"

# на Европе: тестовый сигнал (без Telegram)
venv/bin/python3 bridge_client.py BTCUSDT BUY --ip <IP_ТОКИО>
# на Токио в логе: "udp signal" и далее обработка сигнала
```

### 4. Первый запуск: dry-run → прод

`deploy/.env.example` уже содержит `TRADE_DRY_RUN=true` — движок считает размер
позиции и TP/SL, логирует их, но **не отправляет ордера**.

```bash
# на Токио
sudo journalctl -u rust_trader -f
```

Что должно быть в логе:
- `heartbeat` раз в минуту со счётчиками;
- при сигнале — `[dry-run] market order suppressed` с `qty`, `price`, `notional_usdt`.

Убедились, что объёмы и TP/SL адекватны — переключаемся в прод:

```bash
sed -i 's/^TRADE_DRY_RUN=.*/TRADE_DRY_RUN=false/' .env
sudo systemctl restart rust_trader
```

### 5. Обновление кода на сервере

```bash
cd /opt/rocket
git pull
sudo bash deploy/install.sh                      # пересборка + обновление юнитов
sudo systemctl restart rust_trader price_streamer balance_streamer reset_account_config
# на узле-слушателе:
sudo systemctl restart telegram_listener
```

### 6. Диагностика

```bash
# состояние сервисов
systemctl status rust_trader price_streamer balance_streamer reset_account_config
systemctl status telegram_listener          # на Европе

# логи
journalctl -u rust_trader -f
journalctl -u price_streamer -n 100 --no-pager

# офлайн-самопроверка бинарника (не требует сети)
/opt/rocket/engine_rust/target/release/rust_trader --selftest

# что видит движок в Redis
redis-cli hlen futures_tickers        # цены (должно быть > 0)
redis-cli hget futures_tickers BTCUSDT
redis-cli get balance:<owner>         # баланс аккаунта
redis-cli llen trade_queue            # очередь сигналов
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
| Слушатель писал сигнал только в локальный Redis | в гео-сплите Токио не получал сигналы | опциональная отправка по UDP при заданном `BRIDGE_HOST` |

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

## 🎮 Локальный запуск (для разработки)

```bash
redis-server &                       # локальный Redis
python3 price_streamer.py &
python3 balance_streamer.py &

python3 launcher.py --mode rust      # движок (минимальная задержка)
# альтернативы:
# python3 launcher.py --mode python  # Python-движок
# python3 launcher.py --mode duel    # оба сразу
```

Проверка и отладка Rust-движка:

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
| `generate_session.py` | Listener (Европа) | создаёт Telegram-сессию `sessions/listener_1.session` (интерактивный вход) |

`reset_account_config` запускается отдельным юнитом **после** того, как
`price_streamer` наполнил `futures_tickers`: если тикеров ещё нет, скрипт
логирует ошибку и ждёт следующего цикла (6 часов).

```bash
sudo systemctl start reset_account_config     # вручную, если нужно сейчас
python3 reset_account_config.py               # без systemd — цикл 6 часов
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
- `.env`, `config/api_keys.json` и `sessions/*.session` — секреты, в репозитории
  их нет. Если такой файл когда-либо попадёт в git — перевыпустите ключи.
- `engine_rust/target/` и `venv/` — локальные артефакты сборки, создаются на
  сервере при `deploy/install.sh`, в git не нужны (см. `.gitignore`).

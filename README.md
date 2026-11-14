# Rocket v2 — Telegram Listener и Trading Node

Проект состоит из двух частей:

- **Listener Node (Европа)** принимает сообщения из целевого Telegram-чата, передаёт их в локальную очередь Redis и, если настроен UDP-мост, отправляет копии сигналов на Trading Node.
- **Trading Node (Токио)** получает сигналы, использует Redis с данными о ценах и балансах и запускает торговый движок Rust.

Для гео-разделённой установки эти части работают на разных серверах. На одном сервере можно развернуть обе части, но для рабочей схемы «Европа → Токио» следуйте отдельным инструкциям ниже.

> **Важно:** торговля фьючерсами связана с риском потери средств. На Trading Node оставляйте `TRADE_DRY_RUN=true`, пока не проверены конфигурация, сигналы, размеры позиций и TP/SL. Переключение в live-режим может привести к отправке реальных ордеров.

---

## 1. Архитектура

```text
Telegram
   |
   v
Listener Node (EU)
   |\
   | \  UDP :5005 (если задан BRIDGE_HOST)
   |  \-------------------------------------> Trading Node (Tokyo)
   v                                             |
Redis / trade_queue                              v
                                           Rust trading engine
                                                 |
                                                 v
                                              Binance
```

Listener обрабатывает сообщения только из чата, указанного в `PYROGRAM_TARGET_CHAT`. Для каждого сообщения с текстом или подписью к медиа он:

1. отбрасывает повторное получение того же `message.id` от других Telegram-сессий;
2. записывает сообщение в лог;
3. добавляет текст в Redis list, заданный `REDIS_QUEUE`;
4. при заданном `BRIDGE_HOST` отправляет тот же текст на `BRIDGE_HOST:BRIDGE_PORT` по UDP (три копии датаграммы).

Сообщения без текста и подписи пропускаются. UDP не подтверждает доставку: наличие `BRIDGE_HOST` само по себе не гарантирует, что Trading Node получил пакет.

### Роли серверов

| Сервер | Процессы | Назначение |
|---|---|---|
| Европа — Listener Node | `redis-server`, `telegram_listener` | Приём Telegram-сообщений, локальная очередь и отправка сигналов по UDP |
| Токио — Trading Node | `redis-server`, `price_streamer`, `balance_streamer`, `reset_account_config`, `rust_trader` | Данные Binance, конфигурация аккаунтов и обработка торговых сигналов |

### Сетевые соединения

| Направление | Протокол / порт | Назначение |
|---|---|---|
| Европа → Токио | UDP `5005` | Передача сигналов |
| Европа → Telegram | TCP `443` | Подключение Pyrogram к Telegram |
| Токио → Binance | TCP `443`, TCP `9443` | REST/WebSocket API Binance |
| Каждый сервер локально | TCP `6379` | Redis; не открывать порт в публичный интернет |
| Администрирование | TCP `22` | SSH; ограничить доверенными IP |

На AWS/другом облачном провайдере правила Security Group должны соответствовать правилам локального firewall. На Токио разрешайте входящий UDP `5005` только с IP Listener-сервера.

---

## 2. Структура репозитория

```text
.
├── telegram_listener.py          # Telegram Listener (EU)
├── generate_session.py           # Создание Telegram-сессии (EU)
├── bridge_client.py               # Ручная отправка тестового UDP-сигнала
├── utils/
│   └── logger.py                  # Настройка логирования
├── sessions/                      # Telegram *.session; не коммитить
├── logs/                          # Логи; создаются во время работы
├── requirements_listener.txt      # Python-зависимости Listener
│
├── engine_rust/                   # Rust trading engine (Tokyo)
├── engine_python.py               # Альтернативный Python engine
├── launcher.py                    # Локальный запуск engine
├── price_streamer.py              # Цены Binance → Redis
├── balance_streamer.py            # Балансы Binance → Redis
├── reset_account_config.py        # Настройка плеча и ISOLATED margin
├── config/
│   ├── api_keys.example.json      # Шаблон API-ключей
│   └── api_keys.json              # Локальные секреты; не коммитить
├── requirements_trader.txt        # Python-зависимости Trading Node
│
├── deploy/
│   ├── telegram_listener.service  # systemd unit Listener
│   ├── .env.listener.example      # Шаблон .env для Listener
│   ├── install.sh                 # Установка Trading Node
│   ├── rust_trader.service
│   ├── price_streamer.service
│   ├── balance_streamer.service
│   ├── reset_account_config.service
│   ├── .env.example                # Шаблон .env для Trading Node
│   └── README.md                  # Дополнительная инструкция Trading Node
│
├── .env                           # Локальная конфигурация; не коммитить
└── README.md
```

Для Listener Node не требуется запускать Rust engine, Binance streamers или `reset_account_config.py`. Для Trading Node не требуется запускать Telegram Listener. **Не удаляйте эти исходники из рабочего клона только ради облегчения одного сервера:** они не мешают работе неиспользуемых сервисов, но нужны для обновления репозитория и настройки второго узла.

---

## 3. Подготовка сервера

Инструкции ниже рассчитаны на Ubuntu 20.04/22.04/24.04 amd64 и пользователя с `sudo`.

Получите код проекта в `/opt/rocket`:

```bash
sudo apt update
sudo apt install -y git
sudo mkdir -p /opt/rocket
sudo chown "$USER:$USER" /opt/rocket
git clone https://github.com/dimamalakhov/inside-bot.git /opt/rocket
cd /opt/rocket
```

Если репозиторий уже клонирован в `/opt/rocket`, повторно `git clone` выполнять не нужно:

```bash
cd /opt/rocket
git status
```

Не заменяйте существующий `.env`, `config/api_keys.json` или файлы Telegram-сессий шаблонами без резервной копии.

---

## 4. Listener Node — Европа

### 4.1. Установить системные пакеты и Python-зависимости

`tgcrypto` содержит нативное расширение. На чистом Ubuntu установите компилятор и заголовки Python до установки requirements:

```bash
sudo apt update
sudo apt install -y \
  python3 python3-venv python3-dev build-essential \
  redis-server

cd /opt/rocket
python3 -m venv venv
venv/bin/pip install --upgrade pip setuptools wheel
venv/bin/pip install -r requirements_listener.txt
```

`requirements_listener.txt` устанавливает `pyrofork`, `tgcrypto`, `redis` и `python-dotenv`.

### 4.2. Создать конфигурацию

```bash
cd /opt/rocket
cp deploy/.env.listener.example .env
nano .env
```

Заполните значения:

| Переменная | Назначение |
|---|---|
| `PYROGRAM_API_ID` | Telegram API ID из `my.telegram.org` |
| `PYROGRAM_API_HASH` | Telegram API Hash из `my.telegram.org` |
| `PYROGRAM_TARGET_CHAT` | Числовой ID целевого чата/канала |
| `REDIS_HOST` | Адрес локального Redis; обычно `127.0.0.1` |
| `REDIS_PORT` | Порт Redis; обычно `6379` |
| `REDIS_QUEUE` | Имя Redis list; в примере `trade_queue` |
| `BRIDGE_HOST` | Публичный или приватный IP Trading Node; пустое значение отключает UDP-пересылку |
| `BRIDGE_PORT` | UDP-порт Trading Node; в примере `5005` |

Пример `.env` находится в `deploy/.env.listener.example`. Имена переменных чувствительны к точному написанию: Listener использует `PYROGRAM_TARGET_CHAT`, `REDIS_HOST`, `REDIS_PORT` и `REDIS_QUEUE`.

Защитите конфигурацию и убедитесь, что её может прочитать пользователь systemd:

```bash
sudo chown "$USER:$USER" /opt/rocket/.env
chmod 600 /opt/rocket/.env
```

### 4.3. Создать Telegram-сессию

Создайте каталоги и запустите интерактивную авторизацию от имени того же пользователя, от которого будет работать служба:

```bash
cd /opt/rocket
mkdir -p sessions logs
venv/bin/python3 generate_session.py
```

Скрипт создаёт `sessions/listener_1.session`. Listener загружает все файлы по шаблону `sessions/listener_*.session`.

Если готовые сессии переносятся с другого компьютера, скопируйте их в `/opt/rocket/sessions/`. Не удаляйте `.session` и `.session-journal` при устранении ошибок доступа: это файлы авторизации и SQLite.

### 4.4. Проверить владельца каталогов

Пользователь, указанный в `User=` systemd unit, должен иметь возможность читать/изменять Telegram-сессии и создавать логи. Если служба запускается от текущего пользователя `ubuntu`, выполните:

```bash
sudo mkdir -p /opt/rocket/sessions /opt/rocket/logs
sudo chown -R "$USER:$USER" /opt/rocket/sessions /opt/rocket/logs
```

Не используйте `chmod -R 777`. Если systemd unit настроен на другого пользователя, замените `$USER` на имя этого пользователя.

### 4.5. Настроить и запустить systemd

Команда ниже подставляет `/opt/rocket` и текущего пользователя в шаблон `deploy/telegram_listener.service`:

```bash
cd /opt/rocket
sed -e "s|__APP_DIR__|/opt/rocket|g" \
    -e "s|__RUN_USER__|$USER|g" \
    deploy/telegram_listener.service | \
    sudo tee /etc/systemd/system/telegram_listener.service >/dev/null

sudo systemctl daemon-reload
sudo systemctl enable --now redis-server
sudo systemctl enable --now telegram_listener
```

Проверьте, что в установленном unit указаны правильные `User=` и `WorkingDirectory=`:

```bash
sudo systemctl cat telegram_listener
```

Для этого проекта рабочий каталог службы должен быть `/opt/rocket`, поскольку Listener использует относительный путь `sessions/`.

### 4.6. Проверить Listener

```bash
sudo systemctl status telegram_listener
sudo journalctl -u telegram_listener -n 50 --no-pager
```

Следить за логами в реальном времени:

```bash
sudo journalctl -u telegram_listener -f
```

Файловые логи Listener:

```text
/opt/rocket/logs/session-listener/messages.log
/opt/rocket/logs/session-listener/health.log
```

Проверка Redis:

```bash
redis-cli ping
redis-cli LLEN trade_queue
```

`PONG` подтверждает доступность Redis. `LLEN` показывает текущую длину очереди `trade_queue`; значение `0` само по себе не означает ошибку, например если очередь уже читает потребитель.

### 4.7. UDP-мост в Токио

Если `BRIDGE_HOST` пуст, Listener не отправляет UDP-пакеты. Если указан IP Trading Node, Listener отправляет содержимое сообщения на `BRIDGE_HOST:BRIDGE_PORT` тремя UDP-датаграммами.

На Trading Node порт UDP `5005` должен быть разрешён в локальном firewall и в облачной Security Group только от IP Listener Node.

Для ручного теста на Listener Node существует `bridge_client.py`:

```bash
venv/bin/python3 bridge_client.py BTCUSDT BUY --ip <IP_TRADING_NODE>
```

> **Внимание:** тестовый пакет может быть принят торговым движком как настоящий сигнал. Выполняйте эту проверку только когда на Trading Node включён `TRADE_DRY_RUN=true` и вы проверили, что движок действительно находится в dry-run режиме.

---

## 5. Trading Node — Токио

Эта часть инструкции предназначена только для отдельного торгового сервера.

### 5.1. Настроить секреты

```bash
cd /opt/rocket
cp deploy/.env.example .env
cp config/api_keys.example.json config/api_keys.json
nano .env
nano config/api_keys.json
```

В `.env` настраиваются Redis, торговые параметры, UDP-порт и режим запуска. В шаблоне включён `TRADE_DRY_RUN=true`.

В `config/api_keys.json` указываются API-ключи аккаунтов согласно структуре файла-примера. Не вставляйте секретные ключи в README, сообщения, Git или публичные логи.

### 5.2. Установить Rust engine и сервисы

Установочный скрипт устанавливает системные пакеты, Rust toolchain, собирает Rust engine, создаёт Python virtualenv для стримеров и устанавливает systemd units:

```bash
sudo bash deploy/install.sh
```

По умолчанию сервисы устанавливаются для пользователя `root`. Скрипт также включает и запускает `redis-server`.

### 5.3. Запустить сервисы в правильном порядке

```bash
sudo systemctl start price_streamer balance_streamer
sleep 20
sudo systemctl start reset_account_config
sudo systemctl start rust_trader
```

Порядок важен: сначала стримеры должны начать публиковать цены и балансы в Redis. `reset_account_config` использует данные тикеров для настройки плеча и режима маржи.

### 5.4. Проверить Trading Node

```bash
systemctl status redis-server price_streamer balance_streamer reset_account_config rust_trader
sudo journalctl -u rust_trader -n 100 --no-pager
sudo journalctl -u price_streamer -n 100 --no-pager
sudo journalctl -u balance_streamer -n 100 --no-pager
```

Проверка данных Redis:

```bash
redis-cli HLEN futures_tickers
redis-cli HGET futures_tickers BTCUSDT
redis-cli GET balance:<owner>
```

Замените `<owner>` на реальный идентификатор аккаунта из конфигурации.

### 5.5. Dry-run и live-режим

Не переходите в live-режим, пока не проверены входящие сигналы, API-ключи, баланс, плечо, объём позиции и значения TP/SL.

В шаблоне `deploy/.env.example` установлено `TRADE_DRY_RUN=true`. В этом режиме движок рассчитывает параметры и пишет их в лог, но не должен отправлять реальные ордера. `BINANCE_TESTNET` — отдельная настройка; не путайте её с `TRADE_DRY_RUN`.

Только после отдельной проверки конфигурации можно осознанно изменить `TRADE_DRY_RUN` на `false` и перезапустить `rust_trader`. При `TRADE_DRY_RUN=false` и `BINANCE_TESTNET=false` движок может отправлять реальные ордера в Binance Futures.

Подробности установки Trading Node также описаны в [`deploy/README.md`](deploy/README.md).

---

## 6. Обновление

Перед обновлением сохраните резервные копии `.env`, `config/api_keys.json` и `sessions/`. Эти файлы содержат конфигурацию и/или секреты и не должны заменяться файлами из Git.

### Listener Node

```bash
cd /opt/rocket
git status
git pull
venv/bin/pip install -r requirements_listener.txt
sudo systemctl restart telegram_listener
sudo systemctl status telegram_listener
```

### Trading Node

```bash
cd /opt/rocket
git status
git pull
sudo bash deploy/install.sh
sudo systemctl restart price_streamer balance_streamer reset_account_config rust_trader
```

Если менялись переменные окружения или systemd templates, проверьте конфигурацию и выполните `sudo systemctl daemon-reload` при необходимости.

---

## 7. Диагностика Listener Node

### `ModuleNotFoundError: No module named 'redis'`

Зависимости установлены не в то виртуальное окружение либо установка не завершилась:

```bash
cd /opt/rocket
venv/bin/pip install -r requirements_listener.txt
venv/bin/python3 -c "import redis.asyncio; print('Redis OK')"
```

### `PermissionError: ... /opt/rocket/logs`

Пользователь службы не может создать каталог логов:

```bash
sudo mkdir -p /opt/rocket/logs
sudo chown -R <SERVICE_USER>:<SERVICE_USER> /opt/rocket/logs
```

### `sqlite3.OperationalError: attempt to write a readonly database`

Pyrogram не может обновить SQLite-файл сессии или создать рядом временный journal-файл. Проверьте владельца и права каталога `sessions` и файлов внутри него:

```bash
sudo chown -R <SERVICE_USER>:<SERVICE_USER> /opt/rocket/sessions
ls -lah /opt/rocket/sessions
```

`<SERVICE_USER>` замените на пользователя из `User=` в `sudo systemctl cat telegram_listener`. Не удаляйте `.session` файлы.

### `Не найдено ни одной session-файла` / `No session files`

Проверьте наличие файлов по шаблону:

```bash
ls -lah /opt/rocket/sessions/listener_*.session
```

Если файлов нет, запустите `venv/bin/python3 generate_session.py` от имени пользователя службы.

### `Connected to Redis`, но служба позже завершается

Сообщение о подключении означает только то, что Listener достиг Redis. Посмотрите следующие строки журнала:

```bash
sudo journalctl -u telegram_listener -n 100 --no-pager
```

После запуска Telegram-клиентов в логе ожидаются сообщения `Started ...` и `All systems go. Awaiting messages…`.

### Общие команды управления

```bash
sudo systemctl restart telegram_listener
sudo systemctl stop telegram_listener
sudo systemctl start telegram_listener
sudo systemctl status telegram_listener
sudo journalctl -u telegram_listener -f
```

---

## 8. Безопасность и резервные копии

Не добавляйте в Git и не публикуйте:

- `.env`;
- `config/api_keys.json`;
- `sessions/*.session` и `sessions/*.session-journal`;
- приватные ключи, Telegram API Hash и данные аккаунтов.

В репозитории эти локальные файлы перечислены в `.gitignore`, но перед каждым `git add` всё равно проверяйте `git status`.

Рекомендуется регулярно создавать защищённую резервную копию `.env`, `config/api_keys.json` и Telegram-сессий. Храните копии отдельно от сервера и ограничьте к ним доступ.

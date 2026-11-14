# Inside Bot: развёртывание в Docker

Репозиторий поддерживает два независимых развёртывания Docker Compose: узел Listener в Европе и торговый узел в Токио. Для каждого сервера нужны Ubuntu 22.04 или 24.04, доступ к Git-репозиторию и настроенная группа безопасности AWS. Правила описаны в [`deploy/AWS_FIREWALL.md`](deploy/AWS_FIREWALL.md).

## Архитектура

```text
Telegram ──MTProto/TCP 443──> узел Listener (Pyrogram)
                                  ├── локальная очередь Redis: trade_queue
                                  └── текст по UDP, порт 5005 ──> торговый узел / приёмник Rust
                                                                    ├── локальный Redis (не общий с Европой)
                                                                    ├── price-streamer ──> Redis futures_tickers
                                                                    ├── balance-streamer ──> Redis balance:<owner>
                                                                    └── Rust-движок ──> Binance Futures
```

Listener трижды отправляет исходный текст сообщения Telegram как UDP-пакет UTF-8, сохраняя существующий протокол. Rust также читает локальную очередь `trade_queue` на торговом узле. Redis между серверами не соединяется. Redis узла Listener остаётся локальным — для совместимости и локальных потребителей. Оба Redis работают в частных сетях Compose, данные сохраняются в томах Docker, порт 6379 не публикуется.

На торговом узле запускаются Rust-движок, стримеры цен и балансов и локальный Redis. `engine_python.py` и `launcher.py` сохранены для существующих сценариев, но в этой двухсерверной конфигурации не запускаются. `reset_account_config.py` меняет режим маржи и плечо аккаунтов Binance, поэтому не входит в обычный запуск и включается только отдельным профилем Compose `account-setup`.

## Подготовка серверов

1. Создайте два сервера AWS с Ubuntu 22.04 или 24.04. По возможности используйте стабильные адреса в частной сети VPC.
2. Настройте группы безопасности по [`deploy/AWS_FIREWALL.md`](deploy/AWS_FIREWALL.md). Узнайте частный IP торгового узла и укажите его в `BRIDGE_HOST` на узле Listener.
3. Клонируйте репозиторий на каждый сервер. Ниже для примера используется `/opt/inside-bot`:

   ```bash
   sudo apt update && sudo apt install -y git
   sudo mkdir -p /opt/inside-bot
   sudo chown "$USER":"$USER" /opt/inside-bot
   git clone <адрес-репозитория> /opt/inside-bot
   cd /opt/inside-bot
   ```

   Запускайте установщик через `sudo` из обычной учётной записи — владельца этой копии репозитория. Установщик не отключает systemd-службы и не меняет владельца всего репозитория.

## Узел Listener (Европа)

1. До установки Docker создайте и заполните конфигурацию. Установщик не перезаписывает существующие файлы:

   ```bash
   cp .env.listener.example .env
   nano .env
   ```

2. Заполните параметры:

   | Переменная | Значение |
   |---|---|
   | `PYROGRAM_API_ID` | Числовой ID приложения Telegram на [my.telegram.org](https://my.telegram.org), раздел API development tools. |
   | `PYROGRAM_API_HASH` | Хеш приложения Telegram с той же страницы; храните как секрет. |
   | `PYROGRAM_TARGET_CHAT` | Числовой ID канала или чата, который уже использует Listener. |
   | `REDIS_HOST`, `REDIS_PORT`, `REDIS_QUEUE` | Оставьте значения шаблона: Compose использует свой Redis и существующую очередь `trade_queue`. |
   | `BRIDGE_HOST` | Частный VPC IP торгового узла (предпочтительно) или публичный IP, если частного маршрута нет. |
   | `BRIDGE_PORT` | Оставьте `5005`, если одновременно не меняете настройки обоих узлов и правила firewall. |

3. Выполните `sudo bash deploy/install.sh` и выберите `listener`. Если нет файла `sessions/listener_*.session`, установщик запустит интерактивную генерацию сессии через `generate_session.py` во временном контейнере. Введите в SSH-терминале номер телефона Telegram и одноразовый код. Существующие сессии подключаются как тома и не попадают в Docker-образ.
4. Установщик соберёт образ, проверит Compose, запустит Redis и Listener, затем покажет состояние. Проверить работу можно так:

   ```bash
   sudo bash deploy/status.sh listener
   sudo bash deploy/logs.sh listener
   ```

Не запускайте Docker Listener одновременно с активной `telegram_listener.service`: два процесса не должны использовать одни и те же SQLite-файлы сессий Pyrogram. Установщик обнаружит старую службу и остановится; во время запланированного перехода остановите и отключите её вручную.

## Торговый узел (Токио)

1. Клонируйте тот же репозиторий на сервер Токио и подготовьте конфигурацию:

   ```bash
   cp .env.trader.example .env
   nano .env
   cp config/api_keys.example.json config/api_keys.json
   nano config/api_keys.json
   ```

2. Заполните JSON существующими ключами Binance Futures для торговых групп `founders`, `friends`, `clients` и добавьте аккаунт `support` для стримера цен. Храните файл конфигурации в секрете: он подключается в контейнер только для чтения и исключён из образов и Git. Если файла нет, установщик создаст шаблон, но остановится, пока в нём остаются примеры ключей.
3. Выполните `sudo bash deploy/install.sh` и выберите `trader`. Установщик требует `TRADE_DRY_RUN=true`, соберёт Python/Rust-образ в несколько этапов, проверит Compose и JSON с ключами и запустит Redis, оба стримера и Rust. Сервис настройки аккаунта автоматически не запускается. В режиме dry-run Rust не отправляет торговые ордера.
4. Проверьте состояние и журналы:

   ```bash
   sudo bash deploy/status.sh trader
   sudo bash deploy/logs.sh trader
   sudo bash deploy/logs.sh trader rust-trader
   ```

5. Чтобы отдельно применить настройки плеча и маржи, сначала проверьте аккаунты, конфигурацию и ожидаемые изменения, затем запустите:

   ```bash
   sudo docker compose --env-file .env -f compose.trader.yml --profile account-setup up -d account-config-reset
   ```

   Когда сервис больше не нужен, остановите его командой `sudo docker compose --env-file .env -f compose.trader.yml --profile account-setup stop account-config-reset`.

Rust принимает UDP на `0.0.0.0:<BRIDGE_PORT>` внутри контейнера. Compose публикует только этот UDP-порт. Входящее правило AWS должно разрешать трафик только от узла Listener. Входящий UDP на Listener не нужен.

### Безопасность торговли

В шаблоне указано `TRADE_DRY_RUN=true`, и это обязательное условие первой установки. Rust подключается к Binance и читает рыночные данные и аккаунты, но не отправляет ордера. Python-стримеры цен и балансов используют рабочие адреса Binance и заданные ключи. Python-скрипт `reset_account_config.py` не учитывает режим dry-run Rust, поэтому автоматически не запускается. Перед включением реальной торговли вручную проверьте права API-ключей, файл аккаунтов, объёмы, маршрутизацию сигналов, работу Binance и журналы. Затем отдельно измените `TRADE_DRY_RUN=false` в `.env` и перезапустите `rust-trader`. Установщик откажется запускать торговый узел в реальном режиме.

## Команды Docker и обслуживание

Указывайте роль первым аргументом. Если аргумент пропущен, скрипты читают роль из `.node-role`.

```bash
sudo bash deploy/start.sh listener       # или trader
sudo bash deploy/stop.sh trader          # останавливает контейнеры, не удаляя том Redis
sudo bash deploy/restart.sh trader
sudo bash deploy/status.sh trader
sudo bash deploy/logs.sh trader redis    # если сервис не указан, покажет все журналы
sudo bash deploy/update.sh trader        # обновляет Git, пересобирает и перезапускает; volumes сохраняются
sudo bash deploy/backup.sh listener
sudo bash deploy/backup.sh trader
```

Обновление не удаляет тома Docker и Telegram-сессии. Перед запуском `update.sh` убедитесь, что в локальной копии нет конфликтующих изменений Git. Журналы Docker ротируются: до 10 МБ на файл и ограниченное число копий. Файловые журналы приложения, сессии и конфиги подключены из каталога репозитория. Для каждого узла используется отдельный именованный том Redis.

Резервные копии сохраняются в `backups/` (или `BACKUP_DIR`, если переменная задана): архив содержит конфигурацию и постоянные данные, рядом сохраняется снимок Redis RDB. Во время резервного копирования Listener ненадолго приостанавливается, чтобы корректно скопировать SQLite-сессии. Архивы содержат Telegram-сессии, API-ключи и `.env`; ограничьте к ним доступ и безопасно скопируйте их за пределы сервера. Для восстановления остановите узел, распакуйте архив в корень репозитория и восстановите соответствующий RDB-файл в том Redis. Например, для остановленного торгового узла (замените метки времени на имена своих файлов):

```bash
tar -xzf backups/trader-<метка-времени>.tar.gz -C .
sudo docker volume create inside-bot-trader-redis-data
sudo docker run --rm -v inside-bot-trader-redis-data:/data -v "$PWD/backups:/backup:ro" alpine \
  sh -c 'cp /backup/trader-<метка-времени>-redis.rdb /data/dump.rdb && chown 999:999 /data/dump.rdb'
sudo bash deploy/start.sh trader
```

Для Redis узла Listener используется том `inside-bot-listener-redis-data`. Не восстанавливайте сессию Listener, пока запущен процесс, использующий эту сессию.

## Переменные окружения

Шаблоны ролей описывают переменные приложения. Установщик копирует шаблон, только если `.env` ещё не существует, и не перезаписывает сохранённые настройки.

- Listener: `PYROGRAM_API_ID`, `PYROGRAM_API_HASH`, `PYROGRAM_TARGET_CHAT`, `REDIS_HOST`, `REDIS_PORT`, `REDIS_QUEUE`, `BRIDGE_HOST`, `BRIDGE_PORT`.
- Trader: `REDIS_HOST`, `REDIS_PORT`, `REDIS_DB`, `REDIS_QUEUE`, `REDIS_TICKERS_HASH`, `REDIS_BALANCES_HASH`, `BALANCE_KEY_PREFIX`, `REDIS_DEDUP`, `DUPLICATE_SIGNAL_LOCK_SEC`, `TRADE_DRY_RUN`, `TRADE_MODE`, `BINANCE_TESTNET`, адреса Rust Binance API при необходимости, `API_KEYS_FILE`, `RECV_WINDOW_MS`, `ORDER_TIMEOUT_MS`, `TRADE_ALLOCATION`, `TP_PERCENT`, `SL_PERCENT`, `SL_LIMIT_SLIPPAGE_PERCENT`, варианты `*_BPS`, `BRIDGE_PORT`, `BRIDGE_FORWARD_REDIS`, `RUST_LOG` и параметры настройки кэша Rust. Комментарии в шаблоне содержат значения по умолчанию и пояснения.

В этом проекте Binance-ключи не задаются переменными окружения: приложение читает их из `config/api_keys.json`. Этот файл нужно заполнить отдельно. Не добавляйте секреты в примеры и не коммитьте локальную конфигурацию.

## Группы безопасности AWS

Используйте правила и подстановочные значения из [`deploy/AWS_FIREWALL.md`](deploy/AWS_FIREWALL.md). По возможности направляйте трафик между серверами через частные IP VPC. Только Токио принимает входящий UDP. Не публикуйте Redis. Ограничьте SSH текущим административным IP.

## Переход с systemd

Установщик обнаруживает активные или включённые для загрузки старые службы и завершает работу, не останавливая и не отключая их. Запланируйте переход и вручную остановите и отключите соответствующие службы до запуска Docker:

```bash
# Европа
sudo systemctl stop telegram_listener
sudo systemctl disable telegram_listener

# Токио
sudo systemctl stop rust_trader price_streamer balance_streamer reset_account_config
sudo systemctl disable rust_trader price_streamer balance_streamer reset_account_config
```

Файлы старых служб остаются на месте. Установщик требует, чтобы службы были остановлены и отключены, иначе они могут запуститься рядом с Docker после перезагрузки. Для отката остановите Docker-узел командой `sudo bash deploy/stop.sh <роль>`, затем включите старые службы командой `sudo systemctl enable --now <старые-службы>`. Не запускайте systemd Listener и Docker Listener одновременно с одинаковыми Telegram-сессиями. Не удаляйте тома Docker при откате.

## Диагностика

- **Compose сообщает, что в `.env` не хватает значения:** выполните `bash deploy/check-config.sh <роль>` и заполните обязательные поля. Для Listener обязательны Telegram ID, hash, ID чата, Redis и `BRIDGE_HOST`.
- **Listener не находит файл сессии:** повторно запустите `sudo bash deploy/install.sh` и выберите Listener; установщик создаст сессию, если её нет. Сначала остановите старую службу, если она активна.
- **Ошибка доступа к сессии, журналу или конфигурации:** контейнер работает с UID/GID пользователя, запустившего установку. Проверьте, что этот пользователь может читать и изменять `sessions/` и `logs/`, а также читать `config/api_keys.json`.
- **Rust сообщает, что Redis недоступен:** проверьте `sudo bash deploy/status.sh trader`. Redis локальный, имя Docker-сервиса — `redis`.
- **Сигнал не доходит до Токио:** проверьте, что `BRIDGE_HOST` указывает на Tokyo, порты `BRIDGE_PORT` совпадают, группа безопасности Токио разрешает UDP от IP Listener и `rust-trader` запущен. Redis Listener-to-Trader не используется.
- **Нет рыночных данных или балансов:** изучите журналы `price-streamer` и `balance-streamer`, проверьте права Binance API-ключей, исходящие HTTPS/WSS-соединения, группы `support` и торговые группы в `config/api_keys.json`.
- **Старая systemd-служба активна или включена:** во время запланированного перехода остановите и отключите её вручную. Установщик не меняет состояние существующих служб.

## Что не проверено локально

Установщик на целевом Ubuntu-сервере собирает образы, проверяет Compose, Redis healthcheck и состояние контейнеров; для нового Listener он также создаёт сессию в интерактивном режиме. Эти проверки требуют самого сервера и его сети. Проверка репозитория здесь не подтверждает подключение к Telegram/Binance API, настройки AWS, маршруты VPC или реальную доставку UDP между VPS. Подтвердите их на развёрнутых узлах. Не проверяйте соединение включением реальных ордеров.

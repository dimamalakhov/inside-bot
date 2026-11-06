import asyncio
import json
import logging
# from logging.handlers import RotatingFileHandler
import os
import platform
import time
from pathlib import Path
from typing import Any

import ccxt.pro
import redis.asyncio as redis
from dotenv import load_dotenv
from tabulate import tabulate


BASE_DIR = Path(__file__).resolve().parent
# LOG_DIR = BASE_DIR / "logs" / "prices"
# LOG_DIR.mkdir(parents=True, exist_ok=True)

log = logging.getLogger("prices")
log.setLevel(logging.INFO)

log_formatter = logging.Formatter("%(asctime)s %(levelname)s %(message)s")

# File logging disabled
# file_handler = ...
# log.addHandler(file_handler)

console_handler = logging.StreamHandler()
console_handler.setFormatter(log_formatter)
log.addHandler(console_handler)


load_dotenv(BASE_DIR / ".env")

REDIS_HOST = os.getenv("REDIS_HOST", "localhost")
REDIS_PORT = int(os.getenv("REDIS_PORT", "6379"))
REDIS_DB = int(os.getenv("REDIS_DB", "0"))
TICKERS_HASH = os.getenv("REDIS_TICKERS_HASH", "futures_tickers")
CONFIG_FILE = Path("./config/api_keys.json")
DISPLAY_UPDATES = False


ticker_data: dict[str, dict[str, Any]] = {}

# Кроссплатформенная настройка политики цикла событий для Windows
if platform.system() == 'Windows':
    asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())


def format_symbol_key(symbol: str) -> str:
    base = symbol.split(":")[0]
    return base.replace("/", "")


def resolve_leverage(symbol: str, leverages: dict[str, Any]) -> int:
    info = leverages.get(symbol)
    if not info:
        return 1

    long_lev = info.get("longLeverage")
    short_lev = info.get("shortLeverage")
    if long_lev is not None and short_lev is not None:
        try:
            available = min(float(long_lev), float(short_lev))
        except (TypeError, ValueError):
            available = None
    else:
        try:
            available = float(info.get("info", {}).get("leverage"))
        except (TypeError, ValueError):
            available = None

    if available is None or available <= 0:
        return 1

    return int(available) if available < 10 else 10


async def update_symbol_ticker(exchange, symbol, redis_client, leverages):
    """
    Подписывается на обновления для отдельного символа и сразу обновляет
    глобальный словарь ticker_data и Redis.
    """
    while True:
        try:
            # Получаем обновление для конкретного символа через watch_ticker
            ticker = await exchange.watch_ticker(symbol)
            market = exchange.markets[symbol]
            last = ticker.get("last")
            min_qty = market["limits"]["amount"]["min"]

            leverage = resolve_leverage(symbol, leverages)

            # Формируем данные с меткой времени
            data = {
                "last": last,
                "min_qty": min_qty,
                "leverage": leverage,
                "timestamp": time.time()
            }

            # Преобразуем символ в формат, используемый другими сервисами
            # Например, 'TRX/USDT:USDT' -> 'TRXUSDT'
            redis_symbol_key = format_symbol_key(symbol)

            # Обновляем глобальный словарь и записываем данные в Redis
            ticker_data[redis_symbol_key] = data
            await redis_client.hset(TICKERS_HASH, redis_symbol_key, json.dumps(data))

        except Exception as e:
            # Используем отформатированный ключ в сообщении об ошибке для консистентности
            redis_symbol_key = format_symbol_key(symbol)
            log.warning("Не удалось обновить %s: %s", redis_symbol_key, e, exc_info=True)
            await asyncio.sleep(1)


async def print_updates():
    prev_data = {}
    headers = ['Symbol', 'Last Price', 'Min Qty', 'Leverage', 'Timestamp']
    while True:
        updates = []
        for symbol, data in ticker_data.items():
            # Если данных ранее не было или они изменились
            if symbol not in prev_data or data != prev_data[symbol]:
                row = [
                    symbol,
                    data.get("last"),
                    data.get("min_qty"),
                    data.get("leverage"),
                    time.strftime("%H:%M:%S", time.localtime(data.get("timestamp", 0)))
                ]
                updates.append(row)
        if updates:
            print(tabulate(updates, headers=headers, tablefmt="grid"))
        prev_data = ticker_data.copy()
        await asyncio.sleep(1)


async def main():
    # Инициализируем асинхронного клиента Redis (убедитесь, что параметры подключения совпадают)
    redis_client = redis.Redis(host=REDIS_HOST, port=REDIS_PORT, db=REDIS_DB, decode_responses=True)

    if not CONFIG_FILE.exists():
        log.critical("Файл конфигурации не найден: %s", CONFIG_FILE)
        return

    with open(CONFIG_FILE, "r", encoding="utf-8") as f:
        api_keys_groups = json.load(f)

    # Фильтруем только support аккаунты
    support_keys = api_keys_groups.get('support', [])

    if not support_keys:
        log.error("Нет аккаунтов в группе support!")
        return

    # Используем первый ключ из support группы
    key_data = support_keys[0]

    # Инициализируем биржу Binance для фьючерсной торговли
    exchange = ccxt.pro.binance({
        'apiKey': key_data['public'],
        'secret': key_data['private'],
        'options': {'defaultType': 'future'}
    })
    await exchange.load_markets()

    # Отбираем символы, принадлежащие к USDT-маржинальным (линейным) контрактам
    symbols = [symbol for symbol in exchange.markets.keys() if ':' in symbol and symbol.endswith(':USDT')]
    log.info("Отобранные символы (%s): %s", len(symbols), symbols)

    # Получаем данные по плечу (через REST) для всех символов
    try:
        leverages = await exchange.fetch_leverages(symbols)
    except Exception as e:
        log.warning("Ошибка при получении данных по плечу: %s", e, exc_info=True)
        leverages = {}

    try:
        tasks = []
        # Запускаем отдельную задачу обновления для каждого символа
        for symbol in symbols:
            tasks.append(update_symbol_ticker(exchange, symbol, redis_client, leverages))
        # Задача, выводящая только изменившиеся данные
        if DISPLAY_UPDATES:
            tasks.append(print_updates())
        await asyncio.gather(*tasks)
    finally:
        await exchange.close()
        await redis_client.close()


if __name__ == '__main__':
    asyncio.run(main())
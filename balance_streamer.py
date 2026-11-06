import asyncio
import json
import logging
# from logging.handlers import RotatingFileHandler
import os
import platform
from pathlib import Path
from typing import Dict

import ccxt.pro
import redis.asyncio as redis
from dotenv import load_dotenv


# Non-blocking logger
from utils.logger import setup_logger

BASE_DIR = Path(__file__).resolve().parent
LOG_DIR = BASE_DIR.parent / "logs" / "updater-balances"
LOG_FILE = LOG_DIR / "updater-balances.log"

# Setup Logger (Queue-based, non-blocking)
log, log_listener = setup_logger(
    "balances", 
    log_file=LOG_FILE, 
    console=True
)


load_dotenv(BASE_DIR / ".env")

REDIS_HOST = os.getenv("REDIS_HOST", "localhost")
REDIS_PORT = int(os.getenv("REDIS_PORT", "6379"))
REDIS_DB = int(os.getenv("REDIS_DB", "0"))
CONFIG_FILE = Path("./config/api_keys.json")


if platform.system() == "Windows":
    asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())


global_balances: Dict[str, dict] = {}

async def watch_balance_for_account(owner, exchange, redis_client):
    try:
        # Загружаем рынки для проверки подключения и получения начального баланса
        await exchange.load_markets()
        initial_balance = await exchange.fetch_balance()
        global_balances[owner] = initial_balance
        # Приводим значение к числу и сохраняем как строку
        initial_value = str(float(initial_balance['info']['totalWalletBalance']))
        await redis_client.set(f"balance:{owner}", initial_value)
        log.info("[%s] Начальный баланс: %s", owner, initial_value)
    except Exception as e:
        log.error("[%s] Ошибка при инициализации баланса: %s", owner, e, exc_info=True)

    while True:
        try:
            # Метод watch_balance ждёт обновлений баланса через вебсокет
            balance = await exchange.watch_balance()
            global_balances[owner] = balance
            # Приводим значение к числу и сохраняем как строку
            new_balance = str(float(balance['total']['USDT']))
            await redis_client.set(f"balance:{owner}", new_balance)
            log.info("[%s] Баланс обновлён: %s", owner, new_balance)
        except Exception as e:
            log.error("[%s] Ошибка при получении баланса: %s", owner, e, exc_info=True)
            await asyncio.sleep(5)


async def main():
    # Создаем асинхронное подключение к Redis
    redis_client = redis.Redis(host=REDIS_HOST, port=REDIS_PORT, db=REDIS_DB, decode_responses=True)

    # Загружаем API-ключи из файла
    if not CONFIG_FILE.exists():
        log.critical("Конфигурация не найдена: %s", CONFIG_FILE)
        return

    with open(CONFIG_FILE, 'r', encoding='utf-8') as f:
        api_keys_groups = json.load(f)

    tasks = []
    # Перебираем все группы
    for group_name, group_keys in api_keys_groups.items():
        # Фильтруем только founders, friends и clients
        if group_name in ['founders', 'friends', 'clients']:
            for key in group_keys:
                owner = key['owner']
                api_key = key.get('public')
                secret = key.get('private')

                # Инициализируем экземпляр биржи Binance для фьючерсной торговли
                exchange = ccxt.pro.binance({
                    'apiKey': api_key,
                    'secret': secret,
                    'options': {
                        'defaultType': 'future'
                    },
                })

                task = asyncio.create_task(watch_balance_for_account(owner, exchange, redis_client))
                tasks.append(task)

    try:
        await asyncio.gather(*tasks)
    finally:
        log_listener.stop()

if __name__ == '__main__':
    asyncio.run(main())

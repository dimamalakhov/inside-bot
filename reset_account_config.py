import asyncio
import json
import logging
# from logging.handlers import RotatingFileHandler
import os
import platform
from pathlib import Path
from typing import Any, Dict

import redis.asyncio as redis
from binance import AsyncClient
from binance.exceptions import BinanceAPIException
from dotenv import load_dotenv


BASE_DIR = Path(__file__).resolve().parent
# LOG_DIR = BASE_DIR.parent / "logs" / "setting-account"
# LOG_FILE = LOG_DIR / "settings.log"

# LOG_DIR.mkdir(parents=True, exist_ok=True)

log = logging.getLogger("account_reset")
log.setLevel(logging.INFO)

formatter = logging.Formatter("%(asctime)s %(levelname)s %(message)s")

# File Logging Disabled
# file_handler = ...
# log.addHandler(file_handler)


load_dotenv(BASE_DIR / ".env")

REDIS_HOST = os.getenv("REDIS_HOST", "127.0.0.1")
REDIS_PORT = int(os.getenv("REDIS_PORT", "6379"))
REDIS_DB = int(os.getenv("REDIS_DB", "0"))
TICKERS_HASH = os.getenv("REDIS_TICKERS_HASH", "futures_tickers")
CONFIG_FILE = Path("./config/api_keys.json")
HEARTBEAT_INTERVAL = 30 * 60
MAIN_CYCLE_INTERVAL = 6 * 60 * 60  # Периодичность повторной установки плеч и режима маржи (в секундах)


if platform.system() == "Windows":
    asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())


async def process_account(key_data: Dict[str, Any], tickers_data: Dict[str, str]):
    """
    Обработка плеча и маржи для одного аккаунта с раздельным
    и более надежным хэндлингом ошибок.
    """
    owner = key_data['owner']
    client = None
    try:
        client = await AsyncClient.create(
            api_key=key_data['public'],
            api_secret=key_data['private']
        )
        for symbol, ticker_str in tickers_data.items():
            try:
                cleaned_symbol = symbol.replace('/', '').split(':')[0]
                ticker = json.loads(ticker_str)
                leverage = ticker.get('leverage', 1)

                try:
                    await client.futures_change_margin_type(symbol=cleaned_symbol, marginType='ISOLATED')
                    log.info("[%s] Установлен ISOLATED режим для %s", owner, cleaned_symbol)
                except BinanceAPIException as e:
                    if e.code == -4046:
                        log.info("[%s] Режим для %s уже ISOLATED. Пропускаем.", owner, cleaned_symbol)
                    else:
                        log.error("[%s] Ошибка установки режима маржи для %s: %s", owner, cleaned_symbol, e)
                        continue
                try:
                    await client.futures_change_leverage(symbol=cleaned_symbol, leverage=leverage)
                    log.info("[%s] Установлено плечо %sx для %s", owner, leverage, cleaned_symbol)
                except BinanceAPIException as e:
                    if e.code == -4046:
                        log.info("[%s] Плечо для %s уже %sx. Пропускаем.", owner, cleaned_symbol, leverage)
                    else:
                        log.error("[%s] Ошибка установки плеча для %s: %s", owner, cleaned_symbol, e)
            except json.JSONDecodeError:
                log.error("[%s] Не удалось декодировать JSON для исходного символа %s: %s", owner, symbol, ticker_str)
            except Exception as e:
                log.error("[%s] Неизвестная ошибка обработки символа %s: %s", owner, symbol, e)
    except BinanceAPIException as e:
        if e.code == -2015:
            log.error("Неверные ключи API для аккаунта %s: %s", owner, e)
        else:
            log.error("Ошибка API Binance для аккаунта %s: %s", owner, e)
    except Exception as e:
        log.error("Критическая ошибка для аккаунта %s: %s", owner, e)
    finally:
        if client:
            await client.close_connection()
            log.debug("Соединение для аккаунта %s закрыто.", owner)


async def run_main_cycle(redis_client, api_keys_groups):
    """
    Основной рабочий цикл, устойчивый к ошибкам внутри одной итерации.
    """
    while True:
        try:
            tickers_data = await redis_client.hgetall(TICKERS_HASH)
            if not tickers_data:
                log.error("Данные о тикерах из Redis пусты. Пропускаем итерацию.")
            else:
                tasks = []
                owners_in_iteration = []
                for group_name, group_keys in api_keys_groups.items():
                    if group_name in ['founders', 'friends', 'clients']:
                        for key_data in group_keys:
                            owners_in_iteration.append(key_data['owner'])
                            tasks.append(process_account(key_data, tickers_data))
                if tasks:
                    await asyncio.gather(*tasks)
                    owners_text = ", ".join(owners_in_iteration) if owners_in_iteration else "-"
                    log.info("Итерация завершена. Обработаны аккаунты: %s", owners_text)
                else:
                    log.info("Итерация завершена. Целевых аккаунтов не найдено.")
        except Exception as e:
            log.error("Произошла ошибка в основном цикле. Следующая попытка через 6 часов. Ошибка: %s", e,
                      exc_info=True)
        finally:
            await asyncio.sleep(MAIN_CYCLE_INTERVAL)


async def main():
    """
    Главная функция: инициализирует ресурсы и запускает параллельные задачи.
    """
    redis_client = None
    try:
        redis_client = redis.Redis(host=REDIS_HOST, port=REDIS_PORT, db=REDIS_DB, decode_responses=True)
        await redis_client.ping()

        with open(CONFIG_FILE, 'r', encoding='utf-8') as f:
            api_keys_groups = json.load(f)

        if not any(group in ['founders', 'friends', 'clients'] for group in api_keys_groups):
            log.error("В файле конфигурации не найдено ни одной из целевых групп: founders, friends, clients.")

        main_task = asyncio.create_task(run_main_cycle(redis_client, api_keys_groups))
        await main_task

    except FileNotFoundError:
        log.error("Файл конфигурации не найден по пути: %s", CONFIG_FILE)
    except json.JSONDecodeError as e:
        log.error("Ошибка декодирования JSON в файле конфигурации: %s", e)
    except redis.exceptions.ConnectionError as e:
        log.error("Не удалось подключиться к Redis: %s", e)
    except Exception as e:
        log.error("Критическая ошибка в main, скрипт остановлен: %s", e, exc_info=True)
    finally:
        if redis_client:
            await redis_client.aclose()


if __name__ == '__main__':
    asyncio.run(main())
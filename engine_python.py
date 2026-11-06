import os
import asyncio
import json
import logging
# from logging.handlers import RotatingFileHandler
import platform
import re
import uuid
from decimal import Decimal, ROUND_DOWN, ROUND_UP, InvalidOperation
from pathlib import Path
from typing import Any, Dict, Optional, Tuple, List

import redis.asyncio as redis
from binance import AsyncClient
from binance.exceptions import BinanceAPIException
from dotenv import load_dotenv

BASE_DIR = Path(__file__).resolve().parent
ENV_PATH = BASE_DIR / ".env"
load_dotenv(ENV_PATH)

# --- Конфигурация ---
CONFIG_FILE = BASE_DIR / "config" / "api_keys.json"
LOG_DIR = BASE_DIR / "logs" / "receiver"
LOG_FILE = LOG_DIR / "receiver.log"
REDIS_HOST = os.getenv("REDIS_HOST", "localhost")
REDIS_PORT = int(os.getenv("REDIS_PORT", "6379"))
REDIS_QUEUE = os.getenv("REDIS_QUEUE", "trade_queue")
REDIS_TICKERS_HASH = os.getenv("REDIS_TICKERS_HASH", "futures_tickers")
TRADE_ALLOCATION = Decimal(os.getenv("TRADE_ALLOCATION", "0.3"))
TP_PERCENT = Decimal(os.getenv("TP_PERCENT", "0.3"))
SL_PERCENT = Decimal(os.getenv("SL_PERCENT", "0.3"))
DUPLICATE_SIGNAL_LOCK_SEC = 60

# --- Настройка логов ---
from utils.logger import setup_logger

# Non-blocking console logger (no file logging for engine as per request)
log, log_listener = setup_logger(
    "receiver", 
    log_file=None, 
    console=True
)

# --- Глобальные переменные ---
exchange_info_cache: Dict[str, Dict[str, Any]] = {}
SIGNAL_PATTERN = re.compile(r"#(\w+)\s+(Bullish|Bearish)", re.IGNORECASE)


# --- Data structures ---
if platform.system() == "Windows":
    asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())


class BackgroundCache:
    """
    Класс для локального хранения данных (кэш), обновляемый в фоне.
    Позволяет читать цены и балансы с нулевой задержкой (без запросов в Redis).
    """
    def __init__(self, redis_client: redis.Redis, account_names: List[str]):
        self.redis = redis_client
        self.account_names = account_names
        
        # Локальные кэши
        self.tickers: Dict[str, Dict[str, Any]] = {}
        self.balances: Dict[str, Decimal] = {}
        
        self.running = True

    async def update_tickers_loop(self):
        """Постоянно обновляет локальный кэш тикеров из Redis."""
        log.info("Запущен фоновый процесс обновления тикеров.")
        while self.running:
            try:
                # Получаем все тикеры одним запросом
                all_tickers = await self.redis.hgetall(REDIS_TICKERS_HASH)
                if all_tickers:
                    # Парсим JSON один раз и сохраняем
                    for symbol, data_str in all_tickers.items():
                        try:
                            self.tickers[symbol] = json.loads(data_str)
                        except json.JSONDecodeError:
                            pass
                
                # Частая проверка для актуальности цен (например, каждые 100мс-500мс)
                # Redis локально работает быстро, но лучше не спамить слишком сильно
                await asyncio.sleep(0.5) 
            except Exception as e:
                log.error(f"Ошибка в update_tickers_loop: {e}")
                await asyncio.sleep(1)

    async def update_balances_loop(self):
        """Постоянно обновляет локальный кэш балансов из Redis."""
        log.info("Запущен фоновый процесс обновления балансов.")
        while self.running:
            try:
                for name in self.account_names:
                    balance_str = await self.redis.get(f"balance:{name}")
                    if balance_str:
                        try:
                            self.balances[name] = Decimal(balance_str)
                        except InvalidOperation:
                            pass
                # Балансы меняются реже, можно реже обновлять
                await asyncio.sleep(2.0)
            except Exception as e:
                log.error(f"Ошибка в update_balances_loop: {e}")
                await asyncio.sleep(5)

    def get_ticker(self, symbol: str) -> Optional[Dict[str, Any]]:
        return self.tickers.get(symbol)

    def get_balance(self, account_name: str) -> Decimal:
        return self.balances.get(account_name, Decimal("0"))

    async def start(self):
        asyncio.create_task(self.update_tickers_loop())
        asyncio.create_task(self.update_balances_loop())
    
    def stop(self):
        self.running = False


# --- Вспомогательные функции ---
def adjust_precision(value: [float, str], step_size: str) -> str:
    return f"{Decimal(str(value)).quantize(Decimal(step_size), rounding=ROUND_DOWN)}"


def adjust_price(value: Decimal, tick_size: str, rounding_mode=ROUND_DOWN) -> str:
    quant = Decimal(str(tick_size)).normalize()
    adjusted = value.quantize(quant, rounding=rounding_mode)
    decimal_places = abs(quant.as_tuple().exponent)
    return f"{adjusted:.{decimal_places}f}"


def parse_signal(message: str) -> Optional[Tuple[str, str]]:
    message_lower = message.lower()
    match = SIGNAL_PATTERN.search(message_lower)
    if not match:
        return None

    coin = match.group(1).upper()
    setup = match.group(2).lower()
    side = "BUY" if "bullish" in setup else "SELL"
    return f"{coin}USDT", side


async def place_tp_sl_after_fill(
    client: AsyncClient,
    account_name: str,
    symbol: str,
    order_id: int,
    original_side: str,
    signal_id: str,
) -> None:
    """
    Ожидает исполнения ордера и размещает TP/SL.
    """
    try:
        for _ in range(10):  # ~2 sec
            order_status = await client.futures_get_order(symbol=symbol, orderId=order_id)
            if order_status.get('status') == 'FILLED':
                # Вычисления (вызовы Decimal) быстрые, логов меньше
                avg_price = Decimal(order_status.get("avgPrice", "0"))
                exec_qty = Decimal(order_status.get("executedQty", "0"))

                if not (avg_price > 0 and exec_qty > 0):
                    return

                # Используем кэш инфо (он в памяти)
                symbol_info = exchange_info_cache.get(symbol)
                if not symbol_info:
                    return

                filters = {f["filterType"]: f for f in symbol_info['filters']}
                step_size = filters["LOT_SIZE"]['stepSize']
                price_tick_size = filters["PRICE_FILTER"]["tickSize"]

                tp_rate = Decimal("1") + TP_PERCENT / Decimal("100")
                sl_rate = Decimal("1") - SL_PERCENT / Decimal("100")

                if original_side == 'BUY':
                    bracket_side, tp_price, sl_price = 'SELL', avg_price * tp_rate, avg_price * sl_rate
                    tp_rounding, sl_rounding = ROUND_UP, ROUND_DOWN
                    sl_limit_slippage = Decimal("0.995")
                else:
                    bracket_side, tp_price, sl_price = 'BUY', avg_price * sl_rate, avg_price * tp_rate
                    tp_rounding, sl_rounding = ROUND_DOWN, ROUND_UP
                    sl_limit_slippage = Decimal("1.005")

                formatted_tp_price = adjust_price(tp_price, price_tick_size, tp_rounding)
                formatted_sl_stop_price = adjust_price(sl_price, price_tick_size, sl_rounding)
                sl_limit_price = sl_price * sl_limit_slippage
                formatted_sl_limit_price = adjust_price(sl_limit_price, price_tick_size, sl_rounding)
                formatted_quantity = adjust_precision(exec_qty, step_size)

                # Параллельная отправка TP и SL
                await asyncio.gather(
                    client.futures_create_order(
                        symbol=symbol, side=bracket_side, type='TAKE_PROFIT',
                        price=formatted_tp_price, stopPrice=formatted_tp_price,
                        quantity=formatted_quantity, timeInForce='GTC', reduceOnly=True
                    ),
                    client.futures_create_order(
                        symbol=symbol, side=bracket_side, type='STOP',
                        price=formatted_sl_limit_price, stopPrice=formatted_sl_stop_price,
                        quantity=formatted_quantity, timeInForce='GTC', reduceOnly=True
                    ),
                    return_exceptions=True # Игнорировать ошибки одной чтобы другая прошла
                )
                
                log.info(f"[{signal_id}/{account_name}] TP/SL установлены.")
                return

            await asyncio.sleep(0.2)
            
    except Exception as e:
        log.error(f"[{signal_id}/{account_name}] Ошибка TP/SL: {e}")


async def execute_trade_for_account(
    name: str,
    client: AsyncClient,
    symbol: str, 
    side: str, 
    ticker_data: Dict[str, Any],
    cache: BackgroundCache,
    signal_id: str
):
    """
    Исполняет сделку для одного аккаунта.
    Полностью изолированная логика, чтобы запускать в asyncio.gather.
    """
    try:
        # 1. Получаем баланс из КЭША (0ns network IO)
        available_balance = cache.get_balance(name)
        if available_balance <= 0:
            return

        # 2. Расчеты (CPU bound, fast)
        price = Decimal(str(ticker_data.get("last", "0")))
        leverage = int(ticker_data.get("leverage", 1))
        
        if price <= 0: 
            return

        notional = available_balance * TRADE_ALLOCATION * Decimal(leverage)
        volume = notional / price

        symbol_info = exchange_info_cache.get(symbol)
        if not symbol_info:
            return

        filters = {f["filterType"]: f for f in symbol_info["filters"]}
        min_notional = Decimal(str(filters["MIN_NOTIONAL"]["notional"]))
        min_qty = Decimal(str(filters["LOT_SIZE"]["minQty"]))
        step_size = filters["LOT_SIZE"]["stepSize"]

        if notional < min_notional or volume < min_qty:
            return

        quantity = adjust_precision(volume, step_size)

        # 3. Отправка ордера (Network IO) - самое важное действие
        # Логируем ДО только если это не блокирует, но лучше ПОСЛЕ
        try:
            order = await client.futures_create_order(
                symbol=symbol, side=side, type="MARKET", quantity=quantity
            )
        except Exception as e:
            # Retry logic could be here, but for speed usually one shot is enough or minimal retry
            log.warning(f"[{signal_id}/{name}] Fail 1: {e}")
            return

        # 4. Логирование и TP/SL (Background tasks)
        log.info(f"[{signal_id}/{name}] Ордер {side} {symbol} исполнен (ID: {order.get('orderId')})")
        
        asyncio.create_task(
            place_tp_sl_after_fill(client, name, symbol, order['orderId'], side, signal_id)
        )

    except Exception as e:
        log.error(f"[{signal_id}/{name}] Critical Error: {e}", exc_info=True)


async def listen_trade_signals(redis_client: redis.Redis, binance_clients: Dict[str, AsyncClient], cache: BackgroundCache):
    """Основной цикл (Hot Path)."""
    log.info("Слушатель сделок запущен.")
    while True:
        try:
            # Блокирующее чтение из Redis (это нормально, мы ждем сигнал)
            _, message_body = await redis_client.blpop(REDIS_QUEUE)
            
            # --- START CRITICAL PATH ---
            # Время пошло! Каждая микросекунда на счету.
            
            signal_id = uuid.uuid4().hex[:8]
            # НЕ ЛОГИРУЕМ СРАЗУ В ФАЙЛ (IO), только парсим
            
            parsed = parse_signal(message_body)
            if not parsed:
                continue

            symbol, side = parsed
            
            # Быстрая проверка на дубли через Redis (тут неизбежен IO, но он быстрый, если Redis локальный)
            # Оптимизация: можно вынести это в Lua скрипт или использовать локальный кэш "последних сигналов",
            # но Redis atomic set - надежнее для синхронизации.
            # Если бот один - можно заменить на локальную переменную!
            # Предположим пока Redis для надежности.
            if not await redis_client.set(f"processed_signal:{symbol}_{side}", "1", ex=DUPLICATE_SIGNAL_LOCK_SEC, nx=True):
                continue
            
            # Снимаем лок противоположной стороны (Fire and forget)
            opposite_side = "SELL" if side == "BUY" else "BUY"
            asyncio.create_task(redis_client.delete(f"processed_signal:{symbol}_{opposite_side}"))

            # Получаем цену из КЭША (0ms)
            ticker_data = cache.get_ticker(symbol)
            if not ticker_data:
                log.error(f"[{signal_id}] Нет данных тикера для {symbol}")
                continue

            # --- PARALLEL EXECUTION ---
            # Формируем задачи для ВСЕХ аккаунтов сразу
            tasks = []
            for name, client in binance_clients.items():
                tasks.append(execute_trade_for_account(
                    name, client, symbol, side, ticker_data, cache, signal_id
                ))
            
            # Запускаем все одновременно!
            if tasks:
                asyncio.create_task(asyncio.gather(*tasks))
                
            log.info(f"[{signal_id}] Сигнал {side} {symbol} обработан, задачи запущены.")
            # --- END CRITICAL PATH ---

        except redis.exceptions.ConnectionError:
            await asyncio.sleep(1)
        except Exception as e:
            log.critical(f"Error in listener: {e}", exc_info=True)
            await asyncio.sleep(1)


async def main():
    """Инициализация и запуск бота."""
    log.info("--- Запуск OPTIMIZED торгового приемника ---")

    try:
        redis_conn = redis.Redis(host=REDIS_HOST, port=REDIS_PORT, db=0, decode_responses=True)
        await redis_conn.ping()
    except Exception as e:
        log.critical(f"Redis connect failed: {e}")
        return

    if not CONFIG_FILE.exists():
        log.critical(f"Config not found: {CONFIG_FILE}")
        return

    with open(CONFIG_FILE, "r", encoding="utf-8") as f:
        api_keys = json.load(f)

    accounts_to_trade = (
        api_keys.get("founders", [])
        + api_keys.get("friends", [])
        + api_keys.get("clients", [])
    )
    if not accounts_to_trade:
        log.critical("No accounts to trade.")
        return

    # Предзагрузка информации о символах
    try:
        first_account = accounts_to_trade[0]
        # Используем временный клиент для загрузки exchangeInfo
        temp_client = await AsyncClient.create(first_account["public"], first_account["private"])
        info = await temp_client.futures_exchange_info()
        global exchange_info_cache
        exchange_info_cache = {s["symbol"]: s for s in info["symbols"] if s["status"] == "TRADING"}
        await temp_client.close_connection()
        log.info(f"Exchange info loaded: {len(exchange_info_cache)} symbols.")
    except Exception as e:
        log.critical(f"Failed to load exchange info: {e}")
        return

    # Создание клиентов
    binance_clients = {}
    for acc in accounts_to_trade:
        try:
            client = await AsyncClient.create(acc["public"], acc["private"])
            binance_clients[acc["owner"]] = client
        except Exception as e:
            log.error(f"Failed to create client {acc.get('owner')}: {e}")

    if not binance_clients:
        return
    log.info(f"Created {len(binance_clients)} Binance clients.")

    # Инициализация кэша
    cache = BackgroundCache(redis_conn, list(binance_clients.keys()))
    await cache.start()
    
    # Даем время на первый прогрев кэша
    await asyncio.sleep(2)

    listener_task = asyncio.create_task(listen_trade_signals(redis_conn, binance_clients, cache))

    try:
        await listener_task
    finally:
        log.info("Shutting down...")
        cache.stop()
        log_listener.stop()
        listener_task.cancel()
        for client in binance_clients.values():
            await client.close_connection()
        await redis_conn.close()


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
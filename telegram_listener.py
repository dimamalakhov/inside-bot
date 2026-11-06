import os
import asyncio
import contextlib
import logging
import socket
# from logging.handlers import RotatingFileHandler
from pathlib import Path

from dotenv import load_dotenv

import redis.asyncio as redis
from pyrogram import Client, filters, idle
from pyrogram.handlers import MessageHandler

"""
Multi-session fastest-message listener + Redis publisher + health log
--------------------------------------------------------------------
Key features
~~~~~~~~~~~~
1. **Fastest arrival** — prints each channel message only once (from the quickest session).
2. **Redis enqueue** — immediately pushes the same message into a Redis list queue
   (`trade_queue`) for downstream consumers.
3. **Health logging** — writes periodic status and reconnect events to
   `logs/health.log`; console receives only real channel messages.
4. **Staggered session startup** — mitigates simultaneous login spikes.
"""

# ─── Configuration ──────────────────────────────────────────────────────────────

ENV_PATH = Path(__file__).resolve().parent / ".env"
load_dotenv(ENV_PATH)

def _require_env(name: str) -> str:
    value = os.getenv(name)
    if value is None:
        raise RuntimeError(f"Не установлена обязательная переменная окружения {name}")
    return value


API_ID: int = int(_require_env("PYROGRAM_API_ID"))
API_HASH: str = _require_env("PYROGRAM_API_HASH")
TARGET_CHAT: int = int(_require_env("PYROGRAM_TARGET_CHAT"))

SESSION_DIR = Path("sessions")
SESSION_PREFIX = "listener_"
SESSION_START_DELAY_SEC = 5.0  # Pause between client.start()
HEALTH_INTERVAL_SEC = 60  # Health-check cadence (seconds)

# Redis settings
REDIS_HOST = _require_env("REDIS_HOST")
REDIS_PORT = int(_require_env("REDIS_PORT"))
REDIS_QUEUE = _require_env("REDIS_QUEUE")
REDIS_RECONNECT_SEC = 5  # Back-off between reconnect attempts

# --- UDP bridge (Европа -> Токио) ---------------------------------------------
# Если BRIDGE_HOST задан, каждый сигнал дополнительно уходит в Токио по UDP.
# Полезная нагрузка — тот же текст сообщения, что и в Redis: движок разбирает
# оба формата (`#BTC Bullish` и JSON).
BRIDGE_HOST = os.getenv("BRIDGE_HOST")
BRIDGE_PORT = int(os.getenv("BRIDGE_PORT", "5005"))
BRIDGE_ADDR = (BRIDGE_HOST, BRIDGE_PORT) if BRIDGE_HOST else None
_bridge_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM) if BRIDGE_ADDR else None


def forward_to_bridge(payload: str) -> None:
    """Fire-and-forget дублирование сигнала в Токио (3 копии, как bridge_client.py)."""
    if _bridge_socket is None:
        return
    data = payload.encode("utf-8", "replace")
    for _ in range(3):
        try:
            _bridge_socket.sendto(data, BRIDGE_ADDR)
        except OSError as exc:
            health_logger.debug("UDP bridge send failed: %s", exc)
            break


# Non-blocking logger
from utils.logger import setup_logger

BASE_DIR = Path(__file__).resolve().parent
LOG_DIR = BASE_DIR / "logs" / "session-listener"

MSG_LOG_FILE = LOG_DIR / "messages.log"
HEALTH_LOG_FILE = LOG_DIR / "health.log"

# Setup Loggers (Queue-based, non-blocking)
msg_logger, msg_listener = setup_logger(
    "session_listener.messages", 
    log_file=MSG_LOG_FILE, 
    console=True
)
msg_logger.propagate = False

health_logger, health_listener = setup_logger(
    "session_listener.health", 
    log_file=HEALTH_LOG_FILE, 
    console=True
)
health_logger.propagate = False

# ─── Deduplication + Redis state ───────────────────────────────────────────────
_last_seen_id: int | None = None
_lock = asyncio.Lock()
redis_client: redis.Redis | None = None  # Initialised in init_redis()


# ─── Fast handler ──────────────────────────────────────────────────────────────
async def _mark_if_new(message_id: int) -> bool:
    async with _lock:
        global _last_seen_id
        if message_id == _last_seen_id:
            return False

        _last_seen_id = message_id

        return True


async def fast_handler(client: Client, message):
    """Print & push a channel message if it is first to arrive."""
    if not await _mark_if_new(message.id):
        return

    # Используем caption (подпись к медиа) или text (текстовое сообщение)
    message_content = message.caption or message.text
    if not message_content:
        return  # Пропускаем сообщения без текста/подписи

    msg_logger.info("TG: %s | %s → %s", message.date.isoformat(), client.name, message_content)

    # 1) UDP-мост в Токио (если настроен) — не зависит от локального Redis
    forward_to_bridge(message_content)

    # 2) Redis: downstream automation
    if redis_client is None:
        return  # Redis not ready; skip silently (already logged)

    payload = message_content  # Customize as needed (e.g., JSON)
    try:
        # Fire-and-forget to avoid blocking the event loop
        asyncio.create_task(redis_client.rpush(REDIS_QUEUE, payload))
    except Exception as exc:
        health_logger.error("Redis enqueue failed: %s", exc)


# ─── Health check ──────────────────────────────────────────────────────────────
async def health_check(clients: list[Client]):
    while True:
        online, offline = [], []
        for c in clients:
            if c.is_connected:
                online.append(c.name)
            else:
                offline.append(c.name)
                health_logger.warning("%s disconnected. Reconnecting…", c.name)
                try:
                    await c.connect()
                    health_logger.info("%s reconnected OK", c.name)
                    online.append(c.name)
                    offline.remove(c.name)
                except Exception as e:
                    health_logger.error("Reconnect failed for %s: %s", c.name, e)
            await asyncio.sleep(0)

        health_logger.info("Health — online: %s | offline: %s",
                           ", ".join(online) if online else "-",
                           ", ".join(offline) if offline else "-")
        await asyncio.sleep(HEALTH_INTERVAL_SEC)


# ─── Redis connection management ───────────────────────────────────────────────
async def init_redis() -> redis.Redis | None:
    global redis_client
    while True:
        try:
            rc = redis.Redis(host=REDIS_HOST, port=REDIS_PORT, decode_responses=True)
            await rc.ping()
            health_logger.info("Connected to Redis %s:%s", REDIS_HOST, REDIS_PORT)
            redis_client = rc
            return rc
        except redis.exceptions.ConnectionError as e:
            health_logger.error("Redis connection failed: %s. Retrying in %s s…", e, REDIS_RECONNECT_SEC)
            await asyncio.sleep(REDIS_RECONNECT_SEC)


# ─── Client lifecycle ──────────────────────────────────────────────────────────

async def init_clients() -> list[Client]:
    clients: list[Client] = []
    session_files = sorted(SESSION_DIR.glob(f"{SESSION_PREFIX}*.session"))
    if not session_files:
        raise FileNotFoundError(
            f"Не найдено ни одной session-файла по шаблону {SESSION_PREFIX}*.session в {SESSION_DIR}"
        )

    for session_file in session_files:
        session_path = session_file.with_suffix("")

        c = Client(
            name=str(session_path),
            api_id=API_ID,
            api_hash=API_HASH,
            workdir=".",
            no_updates=False,
            in_memory=False,
        )

        # Группа 0 (высший приоритет): наш основной обработчик для целевого чата.
        c.add_handler(MessageHandler(fast_handler, filters.chat(TARGET_CHAT)), group=0)

        await c.start()
        health_logger.info("Started %s", c.name)
        clients.append(c)
        await asyncio.sleep(SESSION_START_DELAY_SEC)

    return clients


async def shutdown_clients(clients: list[Client]):
    for c in clients:
        with contextlib.suppress(Exception):
            await c.stop()
        health_logger.info("Stopped %s", c.name)


# ─── Main entrypoint ───────────────────────────────────────────────────────────
async def main():
    await init_redis()
    clients = await init_clients()
    health_task = asyncio.create_task(health_check(clients))
    try:
        health_logger.info("All systems go. Awaiting messages…")
        await idle()
    finally:
        health_task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await health_task
        await shutdown_clients(clients)
        if redis_client is not None:
            await redis_client.close()

        msg_listener.stop()
        health_listener.stop()


if __name__ == "__main__":
    asyncio.run(main())
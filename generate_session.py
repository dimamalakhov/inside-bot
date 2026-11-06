"""
Создаёт Telegram-сессию для слушателя (Listener Node).

Запуск из корня проекта:  python3 generate_session.py

Результат: sessions/listener_1.session — именно этот путь ищет
telegram_listener.py (sessions/listener_*.session).
"""

import os
from pathlib import Path

from dotenv import load_dotenv
from pyrogram import Client

BASE_DIR = Path(__file__).resolve().parent
SESSION_NAME = "listener_1"

load_dotenv(BASE_DIR / ".env")

API_ID = int(os.getenv("PYROGRAM_API_ID") or 0)
API_HASH = os.getenv("PYROGRAM_API_HASH") or ""

if not API_ID or not API_HASH:
    raise RuntimeError(
        "PYROGRAM_API_ID и PYROGRAM_API_HASH должны быть заданы в .env "
        "(см. раздел Listener Node в README.md)"
    )

with Client(
    name=SESSION_NAME,
    api_id=API_ID,
    api_hash=API_HASH,
    workdir=str(BASE_DIR / "sessions"),
) as app:
    print(f"Сессия создана: sessions/{SESSION_NAME}.session")

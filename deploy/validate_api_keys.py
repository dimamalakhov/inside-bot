#!/usr/bin/env python3
"""Проверяет формат JSON-конфигурации Binance, не выводя секреты."""

import json
import sys
from pathlib import Path


def main() -> int:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else "config/api_keys.json")
    try:
        config = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"Некорректная конфигурация аккаунтов Binance в {path}: {exc}", file=sys.stderr)
        return 1

    if not isinstance(config, dict):
        print("Конфигурация аккаунтов Binance должна быть JSON-объектом", file=sys.stderr)
        return 1
    trading = []
    for group in ("founders", "friends", "clients"):
        entries = config.get(group, [])
        if not isinstance(entries, list):
            print(f"Группа аккаунтов '{group}' должна быть JSON-массивом", file=sys.stderr)
            return 1
        trading.extend(entries)
    support = config.get("support", [])
    if not isinstance(support, list) or not support:
        print("Для price_streamer.py требуется хотя бы один аккаунт в группе support", file=sys.stderr)
        return 1
    if not trading:
        print("Добавьте хотя бы один торговый аккаунт в founders/friends/clients", file=sys.stderr)
        return 1

    for group_name, entries in (("trading", trading), ("support", support)):
        for index, entry in enumerate(entries):
            if not isinstance(entry, dict) or not all(
                isinstance(entry.get(field), str) and entry[field].strip()
                for field in ("owner", "public", "private")
            ):
                print(
                    f"В каждой записи {group_name} должны быть заполнены owner/public/private (запись {index + 1})",
                    file=sys.stderr,
                )
                return 1
            if any(token in entry["public"] + entry["private"] for token in ("_HERE", "CHANGE_ME", "YOUR_")):
                print(f"В записи {group_name} {index + 1} найден текст-заполнитель вместо ключа", file=sys.stderr)
                return 1

    print(f"Конфигурация Binance проверена: торговых аккаунтов — {len(trading)}, support — {len(support)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

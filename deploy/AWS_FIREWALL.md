# Правила AWS Security Groups

Замените подстановочные значения фактическими адресами. Предпочтительно использовать частный маршрут VPC между узлами; не открывайте UDP-мост Listener-to-Trader всему интернету. Security Groups отслеживают состояние соединений, поэтому ответный трафик разрешённых подключений пропускается автоматически.

| Сервер | Направление | Протокол | Порт | Источник / назначение | Для чего нужен |
|---|---|---|---|---|---|
| Listener (Европа) | Входящий | TCP | 22 | `<ADMIN_PUBLIC_IP>/32` | Только для административного SSH-доступа. |
| Trading (Токио) | Входящий | TCP | 22 | `<ADMIN_PUBLIC_IP>/32` | Только для административного SSH-доступа. |
| Trading (Токио) | Входящий | UDP | `${BRIDGE_PORT}` (по умолчанию `5005`) | `<LISTENER_PRIVATE_IP>/32` или Security Group Listener | Приём сигналов от Listener на UDP-сокет Rust. Это единственное приложение, которому нужен входящий доступ. |
| Listener (Европа) | Входящий | — | — | Нет | Входящий UDP не требуется: узел сам отправляет UDP-пакеты. |
| Оба сервера | Входящий | TCP | 6379 | Нет | Redis доступен только в частной сети Docker на каждом сервере. Не добавляйте для него правило AWS. |
| Listener (Европа) | Исходящий | TCP | 443 | `0.0.0.0/0` (и `::/0`, если включён IPv6) | Telegram MTProto по TLS, а также HTTPS для обновления ОС, Docker и Git. Pyrogram использует TCP 443 для подключения к производственным серверам Telegram. |
| Listener (Европа) | Исходящий | UDP | `${BRIDGE_PORT}` (по умолчанию `5005`) | `<TRADING_PRIVATE_IP>/32` или `<TRADING_PUBLIC_IP>/32`, если нет маршрута VPC | Односторонний UDP-мост в Токио. |
| Trading (Токио) | Исходящий | TCP | 443 | `0.0.0.0/0` (и `::/0`, если включён IPv6) | Binance REST и WebSocket API, а также HTTPS для обновления ОС, Docker и Git. Для фильтрации по доменам используйте AWS Network Firewall или прокси. |
| Оба сервера | Исходящий | UDP и TCP | 53 | `<CUSTOM_DNS_RESOLVER_IP>/32` | Требуется только при использовании собственного DNS-сервера вместо AmazonProvidedDNS. |

## Примечания

- Стандартные AWS Security Groups не принимают имена доменов в качестве назначения. IP-адреса Telegram и Binance могут меняться, поэтому практичное правило Security Group — разрешить исходящий TCP 443. Для фильтрации по доменам используйте AWS Network Firewall или исходящий прокси.
- AWS Security Groups не могут блокировать DNS-запросы к AmazonProvidedDNS / Route 53 Resolver (адрес VPC+2), поэтому для стандартного VPC DNS отдельное правило не нужно. Для фильтрации DNS используйте [Route 53 Resolver DNS Firewall](https://docs.aws.amazon.com/vpc/latest/userguide/resolver-dns-firewall.html). Правило TCP/UDP 53 из таблицы нужно только при собственном DNS-сервере.
- В Rust по умолчанию настроены Binance Futures WebSocket API `wss://ws-fapi.binance.com/ws-fapi/v1` и REST `https://fapi.binance.com`, оба используют TCP 443. См. [документацию Binance USDⓈ-M Futures WebSocket API](https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-s-m-futures/api/ws-api/user-data-streams). Python-стримеры CCXT подключаются к Binance Futures REST/WebSocket API; существующая инструкция по развёртыванию также указывает TCP 9443 для рыночных потоков. Если используемый путь или библиотека обращается к `fstream.binance.com:9443`, добавьте на Токио исходящее TCP-правило 9443. Входящее правило для него не открывайте.
- Производственное подключение Pyrogram к Telegram использует TCP 443 ([документация Pyrogram](https://docs.pyrogram.org/releases/0/v0.7.1)). В коде не настроен Telegram-прокси или фиксированный список IP Telegram.
- Если возможно, разместите Listener и Trading в одной VPC или в связанных VPC. В `.env` Listener укажите в `BRIDGE_HOST` частный адрес Токио. Входящий UDP на Токио разрешите только от частного IP Listener или его Security Group.
- Compose публикует только UDP-порт `${BRIDGE_PORT}` на торговом узле. Внутренний порт Redis наружу не публикуется. Не добавляйте входящие правила AWS для Redis, HTTP-приложения или других ненужных портов.
- Сужайте исходящие правила после проверки установки пакетов, Git, DNS, Telegram, Binance REST и WebSocket. Для исходящих HTTPS/WSS-соединений входящие правила не требуются.
- При изменении `BRIDGE_PORT` обновите `.env` обоих узлов, входящее правило Токио и исходящее правило Listener.

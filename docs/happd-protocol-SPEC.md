# Happ daemon (happd) — реверс-спека для собственного клиента

Цель: `happd` v4.7.2-341 (x86_64, Qt6, не stripped), SHA256 `6af7401c857ad976108415cf74b7378a2d19235f473ed04a9b2cfc14294a891a`.
Методы: символы `nm`, строки, декомпиляция Ghidra 12.2 через REA.

## Транспорт
- Unix-сокет `/tmp/happd.sock` (паттерн `/tmp/%1.sock`), права выставляются явно (chmod сокета).
- Кадр: **u32 big-endian длина + UTF-8 JSON** (Ghidra: `ClientConnection::createFrame` = QJsonDocument::toJson + QDataStream BE u32 → QByteArray).
- Клиент шлёт `heartbeat` периодически; нет heartbeat за N сек → сокет считается протухшим, процессы клиента чистятся.
- `client-hello` первым: `{action:"client-hello", <client-version>, observer?:bool}`; версия клиента > версии демона → демон сам рестартует себя (systemd Restart=always). `observer:true` — read-only наблюдатель (для панели!).
- Сессия клиента владеет запущенными процессами; при дисконнекте/таймауте — teardown, кроме случая suspend (wake-grace 15 с + `reclaim-session-processes` от нового клиента).

## Действия (action)
`client-hello` · `heartbeat` · `start` · `stop` · `kill` · `close-stdin` · `get-privilege` · `clear-logs` · `reclaim-session-processes` · `start-wg` · `stop-wg` · `wg-status` · `start-process-explorer` · `stop-process-explorer` · `process-explorer-config` · `process-explorer-status` · `shutdown` (broadcast от установщика).

## Ключи запроса start (ProcessManager::validateProcessRequest)
`process-id` (например `happ-xray`, `happ-tun`, `happ-default-tun`, `tun2proxy`, `sing-box`), `executable`, `arguments`, `working-dir`, `environment`, `privileges`, `auto-restart`, `max-restart-attempts`, `restart-delay-ms`, `stdin-data`.

## Ответы/события
- Ответ: `{success, action, message, request-id}` (+ данные: `start-time`, `stdout`, `stderr`; `privilege-level`, `daemon-version` — на get-privilege/hello).
- События процесса (владельцу): состояния `started|stopping|stopped|restarting|crashed|processing` (+ exit code); широковещательные: `wg-dead`, shutdown.
- process-explorer (per-app routing): конфиг `interval_ms`, `include_icons`, `icon_size`; worker перечисляет процессы+иконки (для rules «приложение → прокси»).

## Привилегии
- happd работает root'ом (юнит без sandbox, логи `/var/log/happd.log`).
- PrivilegeManager динамически пишет polkit-политику `/usr/share/polkit-1/actions/com.<vendor>.<app>.policy` (`allow_active=yes`, `exec.allow_gui=true`), эскалация через `/usr/bin/pkexec` (fallback sudo).

## Состояние
- SQLite `/var/lib/happd/state.db` — только `pending_push` (FCM-пуши, MCS/checkin protobuf'ы для push-уведомлений, AES-128-GCM в клиенте).

## Клиент (Happ, stripped) —观察
- Конфиг `~/.config/happ`, данные `~/.local/share/happ`; HWID = `/var/lib/dbus/machine-id`.
- Схемы ссылок: `vless:// vmess:// trojan:// socks:// hysteria2://` + `happ://{add,connect,disconnect,toggle,open,close,routing,on/off,crypt}` deep links.
- Подписки: полные (URL, refresh, user-agent, subscriptionId/Name...).
- Xray-конфиг генерится клиентом; бандл: `xray`, `sing-box` (legacy TUN), `tun2proxy`, `antifilter` (фрагментация), geoip/geosite dat.
- 127.0.0.1:11111 — внутренняяmixed-порт константа.

## Известные болячки под Hyprland (из happ-nixos)
- Бандл Qt wayland-shell-integration крашит на wlroots → удалять/XCB.
- Пустой HWID без `/var/lib/dbus/machine-id` (dbus-broker) — на Omarchy симлинк есть.
- TUN: rp_filter=loose обязателен.

## Проект (решения пользователя)
Свой открытый клиент поверх xray-core + TUN + QML-модуль в панель Omarchy; репо на GitHub пользователя.

# omaxy

Открытый Xray-прокси клиент для **Omarchy/Hyprland**: root-демон (TUN), CLI,
модуль панели Omarchy (QML) и GUI. Протокол демона **wire-совместим с Happ
happd** (u32 BE + JSON, те же action'ы) — оригинальный Happ GUI может работать
с omaxyd и наоборот.

## Зачем
[Happ](https://github.com/Happ-proxy) — популярный клиент к xray-core, но **закрытый**
(proprietary бинари). omaxy восстанавливает функциональность поверх открытого
[xray-core](https://github.com/XTLS/Xray-core) с реверс-инженерным протоколом управления
([спека](docs/happd-protocol-SPEC.md), получена Ghidra-декомпиляцией нет-stripped `happd`).

## Архитектура
```
omaxy GUI/QML-панель ──CLI──▶ omaxy CLI ──u32BE+JSON unix-sock──▶ omaxyd (root)
                                                                  ├─ xray run -c … (TUN+socks+http)
                                                                  └─ sysctl rp_filter/ip_forward для TUN
/tmp/happd.sock  ← так же понимает оригинальный Happ client
```

## Быстрый старт (Arch/Omarchy)
```sh
cargo build --release
sudo ./packaging/install-privileged.sh   # xray + omaxyd.service (сокет /tmp/happd.sock)
./packaging/install-user.sh              # omaxy в ~/.local/bin + модуль панели
omarchy restart shell

omaxy server add 'vless://…#Мой сервер'  # или omaxy subscribe add <url>
omaxy connect                            # TUN + socks 127.0.0.1:10808
omaxy status
```
В панели: иконка 🌐 справа — клик открывает выбор сервера и in/out,
правый клик — быстрый in/out.

## Протокол демона (совместимость)
Кадр: `u32 big-endian длина + UTF-8 JSON` в unix-сокете (по умолчанию `/tmp/happd.sock`).
Действия: `client-hello, heartbeat, start, stop, kill, close-stdin, get-privilege,
clear-logs, reclaim-session-processes, shutdown` (+ собственные `omaxy-put`, `omaxy-status`).
Запрос `start`: `process-id, executable, arguments, environment, auto-restart,
max-restart-attempts, restart-delay-ms` + **расширение `detached:true`** (не умирать
вместе с CLI-сессией; stock happd ключ игнорирует). Процессы принадлежат сессии
клиента и убиваются при его отвале (сеансовая семантика Happ; heartbeat-таймаут 60 с).

## Безопасность
Демон работает от root без песочниц (нужно для TUN — как happd) и принимает
подключения от любого локального пользователя по сокету 0666 (то же у happd).
Если это неприемлемо — ставьте `OMAXY_SOCK=/run/omaxy/omaxy.sock` и группу в
сокет-политике своей системы. Не ставьте omaxyd рядом с активным happd: конфликт сокета.

## Статус
- [x] демон: запуск/остановка процессов, авто-рестарт, сессии, heartbeat
- [x] ссылки vless/vmess/trojan/ss (+hy2 парсинг, конфиг-генерация hy2 — TODO)
- [x] подписки (base64-список / JSON)
- [x] декод ссылок из QR-картинки (`omaxy qr`)
- [x] QML-модуль панели Omarchy
- [ ] GUI omaxy-gui (Slint)
- [ ] конфиг-генерация hysteria2, routing-правила geosite, per-app routing

## Лицензия
MIT. Xray-core — MPL-2.0 (отдельный пакет `xray`). Реверс протокола — образовательная
реконструкция интерфейса; код написан с нуля, байты Happ не переупаковываются.

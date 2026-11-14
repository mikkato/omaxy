# Happ 4.7.2 — реверс-инжиниринг (Ghidra 12.2 headless + carving ресурсов)

Метод: Ghidra (проект `~/gh-proj/happ1`, скрипты `Extract.java`/`Decomp.java`), 
carve встроенных шаблонов из `Happ` (`~/RE-happ/carve/templates.clean.txt`), 
строки Qt-метаданных. Бинарники: `~/.hermes/cache/scratch/happ2/opt/happ/bin/`.

## Компоненты (bin/)
| Файл | Что это |
|---|---|
| `Happ` | GUI Qt6 (QML), генераторы конфигов, polkit-setup |
| `happd` | Qt6 демон, сокет-сервер, ProcessManager, WireGuard-модуль (`WgTun_linux.cpp`, `WgNoise.cpp`), push (`FcmRegistrar`, `McsClient`), state `/var/lib/happd/state.db`, лог `/var/log/happd.log` |
| `core/xray` + geoip/geosite | ядро по умолчанию (SOCKS/HTTP локально) |
| `tun/sing-box` | TUN-движок «singbox» |
| `tun2/tun2proxy-bin` + `libtun2proxy.so` + `udpgw-server` | альт. TUN «tun2proxy» + LD_PRELOAD per-app без root |
| `antifilter/antifilter` | local DPI-desync прокси (fake-sni, split, tlsrec; порт ~1080) |

## Две TUN-цепочки (режим выбирается meta-параметром подписки `tun-type`: `singbox` | `tun2proxy`)

### singbox (основной)
GUI собирает конфиг sing-box (шаблон вырезан дословно):
- `inbounds.tun`: `address=[172.18.0.1/30 (, fd00::1/126)], mtu 1500, auto_route: true, strict_route: true, stack: mixed|system|gvisor`
- `outbounds`: `socks "proxy"` на 127.0.0.1:xray-socks (udp_fragment, domain_resolver dns-proxy prefer_ipv4) + `direct "direct"`
- `route`: `final: proxy, auto_detect_interface: true`, правила:
  1. `process_name: [xray, sing-box, antifilter (+.exe)] → direct` — защита петли;
  2. `action: sniff`; 3. `protocol: dns → action: hijack-dns`.
- `dns.servers`: `{tag dns-proxy, address 8.8.8.8, detour proxy}`; патчи из JSON подписки
  («DNS from JSON»), `tunDnsEnable=true` → detour direct; `strategy ipv4_only` при routing+IPv6.
- Per-app: патчи маршрута `process_name` / `process_path` / `process_path_regex` с outbound
  direct или reject (block-list приложений); «excluded-routes» правило: `ip_cidr LAN → direct`.
- Запуск: GUI шлёт демону `start-process` (program/args/env в JSON, **конфиг передаётся через stdin-data**, не на диск), демон ждёт появления интерфейса по логам, рестартует при `tun-lost`.

### tun2proxy
`tun2proxy-bin` через **pkexec** + одноразовый polkit-setup (создаёт `/usr/local/bin/happ-tun2proxy-wrapper`
(`ulimit -n 65536; exec ...`), kill-хелпер (SIGTERM→2с→SIGKILL), `/etc/polkit-1/actions/com.happ.tun2proxy.policy`,
allow_active=yes). LAN-обход: `--bypass <CIDR>`. Адрес tun `172.19.0.1/30` (+`fdfe:dcba:9877::1/126`), routeOnly, autoSystemRoutingTable.

## xray (core-режим)
Локальные inbound'ы SOCKS/HTTP; `log.loglevel` из настроек; ECH (`echConfigList`), ws headers,
`domainStrategy: IPIfNonMatch`, stats; fakedns + dns-in/dns-out; «socks-direct» outbound;
`Connected without routing (geo files bypassed)` — деградация без geo.

## Прочее в функционале Happ
- process-explorer: сервис списка процессов/приложений (иконки) — источник per-app списка в UI.
- blockList / blockSites / blockSubscription — списки блокировок (реклама/трекинг), `[BlockedDomains] Subscription blocked`.
- DNSTT smoke-пробер резолверов; push-уведомления (FCM/MCS); suspend/resume хуки;
  update-checker; QR; speedtest; «fingerprint»-настройки.

## Почему Happ не вешает систему (наш баг)
1. sing-box `strict_route+auto_route` держит маршрутизацию сам; никакого `resolvectl default-route` на туннель;
2. DNS перехватывается внутри туннеля (`hijack-dns`), наружу ходит только через детуры sing-box;
3. собственные процессы ядра исключены `process_name→direct` (не cgroup-фокусами).

## План переноса в omaxy (1:1)
1. `tun_engine: singbox` по умолчанию: omaxy запускает xray (только socks/http) + sing-box с точным шаблоном Happ; отказ от самописных ip-rule/resolvectl/DNS-обходов в этом режиме.
2. В process_name→direct список: `omaxy, xray, sing-box` + настраиваемые (например `Hermes`).
3. Per-app bypass = правила sing-box `process_name/process_path`, не cgroup.
4. Excluded routes (LAN) = правило `ip_cidr → direct`.
5. Позже: ECH, sniff, blocklist, antifilter-цепочка, tun2proxy-режим, polkit-setup.

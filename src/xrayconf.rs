//! Generate xray-core JSON config from a Server + Settings.
use crate::links::{Server, ServerKind};
use crate::store::Settings;
use serde_json::{json, Map, Value};

pub fn build(server: &Server, st: &Settings, with_tun: bool) -> Value {
    let outbound = build_outbound(server);

    let mut inbounds: Vec<Value> = Vec::new();
    if st.socks_port > 0 || st.http_port > 0 {
        inbounds.push(json!({
            "tag": "socks",
            "port": st.socks_port,
            "listen": "127.0.0.1",
            "protocol": "socks",
            "settings": {"udp": true, "allowTransparent": false}
        }));
        if st.http_port > 0 {
            inbounds.push(json!({
                "tag": "http",
                "port": st.http_port,
                "listen": "127.0.0.1",
                "protocol": "http",
                "settings": {}
            }));
        }
    }
    if with_tun {
        inbounds.push(json!({
            "tag": "tun-in",
            "port": 0,
            "listen": "0.0.0.0",
            "protocol": "tun",
            "settings": {
                "network": "tun",
                "name": "omaxy0",
                "address": [server_tun_address(st)],
                // xray >=25: gateway — массив CIDR, префикс берём из tun_address
                "gateway": [format!(
                    "{}/{}",
                    st.tun_gateway,
                    st.tun_address.split('/').nth(1).unwrap_or("30")
                )],
                "mtu": 9000,
                // маршруты навешивает демон (policy table 88), xray не трогает систему
                "strictRoute": false
            }
        }));
    }

    let mut rules: Vec<Value> = Vec::new();
    if with_tun {
        // только UDP:53; иначе весь TCP:443 из tun попадал в dns-out
        rules.push(json!({"type":"field","inboundTag":["tun-in"],"network":"udp",
                          "port":"53","outboundTag":"dns-out"}));
    }
    if !with_tun {
        // режим singbox: собственный DNS xray должен ходить НАПРЯМУЮ, иначе
        // резолв адреса сервера уходит в туннель и зацикливается (record not found)
        rules.push(json!({"type":"field","network":"udp","port":"53","outboundTag":"direct"}));
    }
    if st.route_mode != "global" {
        rules.push(json!({"type":"field","ip":["0.0.0.0/8","10.0.0.0/8","100.64.0.0/10","127.0.0.0/8","169.254.0.0/16","172.16.0.0/12","192.168.0.0/16","224.0.0.0/4","::1/128","fc00::/7","fe80::/10"],"outboundTag":"direct"}));
        rules.push(json!({"type":"field","domain":["^domain:\\.(lan|local)$","geoip:private"],"outboundTag":"direct"}));
    }

    let dns_servers: Vec<Value> = st.dns.iter().map(|d| json!({"address": d})).collect();

    json!({
        "log": {"loglevel": "warning"},
        "inbounds": inbounds,
        "outbounds": [outbound,
            {"tag":"direct","protocol":"freedom","settings":{}},
            {"tag":"dns-out","protocol":"dns","settings":{}}],
        "dns": {"servers": dns_servers, "tag": "dns"},
        "routing": {"domainStrategy": "AsIs", "rules": rules}
    })
}

fn server_tun_address(st: &Settings) -> String {
    st.tun_address.clone()
}

fn stream_settings(server: &Server) -> Value {
    let net = server.network.clone().unwrap_or_else(|| "tcp".into());
    let sec = server.security.clone().unwrap_or_else(|| "none".into());

    let mut tls: Option<Value> = None;
    if sec == "tls" || sec == "reality" {
        let obj = if sec == "reality" {
            json!({
                "serverName": server.sni.clone().unwrap_or_default(),
                "fingerprint": server.fp.clone().unwrap_or_else(|| "chrome".into()),
                "publicKey": server.pbk.clone().unwrap_or_default(),
                "shortId": server.sid.clone().unwrap_or_default(),
                "spiderX": "",
                "show": false
            })
        } else {
            json!({
                "serverName": server.sni.clone().unwrap_or_default(),
                "fingerprint": server.fp.clone().unwrap_or_else(|| "chrome".into()),
                "alpn": ["h2", "http/1.1"],
                "allowInsecure": server.insecure,
                "show": false
            })
        };
        let mut m = Map::new();
        m.insert("show".into(), json!(false));
        m.insert("fingerprint".into(), json!(server.fp.clone().unwrap_or_else(|| "chrome".into())));
        if sec == "reality" {
            m.insert("publicKey".into(), json!(server.pbk.clone().unwrap_or_default()));
            m.insert("shortId".into(), json!(server.sid.clone().unwrap_or_default()));
            m.insert("serverName".into(), json!(server.sni.clone().unwrap_or_default()));
            tls = Some(json!({"reality": obj, "show": false}));
        } else {
            m.insert("serverName".into(), json!(server.sni.clone().unwrap_or_default()));
            m.insert("allowInsecure".into(), json!(server.insecure));
            tls = Some(json!({"tls": obj, "show": false}));
        }
        let _ = m;
    }

    let sockopt = json!({"tcpNoDelay": true, "domainStrategy": "UseIP"});

    let grpc_service = server.service_name.clone().unwrap_or_default();
    let path = server.path.clone().unwrap_or_default();
    let effective_http_path = if net == "grpc" { path.clone() } else { path.clone() };
    let headers: Value = Value::Object(server.headers.clone());

    match net.as_str() {
        "grpc" => json!({
            "network": "grpc",
            "security": if tls.is_some() { sec.as_str() } else { "none" },
            "grpcSettings": {
                "serviceName": grpc_service,
                "multiMode": effective_http_path == "multi",
                "authority": server.host.clone().unwrap_or_default(),
            },
            "sockopt": sockopt,
            "tls": tls.unwrap_or(json!(null))
        }),
        "ws" => json!({
            "network": "ws",
            "security": if tls.is_some() { sec.as_str() } else { "none" },
            "wsSettings": {
                "path": path,
                "host": server.host.clone().unwrap_or_default(),
                "headers": headers
            },
            "sockopt": sockopt,
            "tls": tls.unwrap_or(json!(null))
        }),
        _ => json!({
            "network": "tcp",
            "security": if tls.is_some() { sec.as_str() } else { "none" },
            "realitySettings": if sec == "reality" {
                json!({"fingerprint": server.fp.clone().unwrap_or_else(|| "chrome".into()),
                       "publicKey": server.pbk.clone().unwrap_or_default(),
                       "shortId": server.sid.clone().unwrap_or_default(),
                       "serverName": server.sni.clone().unwrap_or_default(),
                       "spiderX": ""})
            } else { Value::Null },
            "tlsSettings": if sec == "tls" {
                json!({"serverName": server.sni.clone().unwrap_or_default(),
                       "fingerprint": server.fp.clone().unwrap_or_else(|| "chrome".into()),
                       "allowInsecure": server.insecure})
            } else { Value::Null },
            "sockopt": sockopt
        })
    }
}

fn build_outbound(server: &Server) -> Value {
    match server.kind {
        ServerKind::Vless => json!({
            "tag": "proxy",
            "protocol": "vless",
            "settings": {
                "vnext": [{
                    "address": server.address,
                    "port": server.port,
                    // xray >=25: vnext.users[{id, encryption, flow}] — ровно один пользователь
                    "users": [{
                        "id": server.uuid,
                        "encryption": server.encryption.clone().unwrap_or_else(|| "none".into()),
                        "flow": server.flow.clone().unwrap_or_default(),
                        "level": 8
                    }]
                }]
            },
            "streamSettings": stream_settings(server)
        }),
        ServerKind::Vmess => json!({
            "tag": "proxy",
            "protocol": "vmess",
            "settings": {
                "vnext": [{
                    "address": server.address,
                    "port": server.port,
                    "users": [{
                        "id": server.uuid,
                        "alterId": 0,
                        "security": server.method.clone().unwrap_or_else(|| "auto".into()),
                        "level": 8
                    }]
                }]
            },
            "streamSettings": stream_settings(server)
        }),
        ServerKind::Trojan => json!({
            "tag": "proxy",
            "protocol": "trojan",
            "settings": {
                "servers": [{
                    "address": server.address,
                    "port": server.port,
                    "password": server.uuid,
                    "level": 8
                }]
            },
            "streamSettings": stream_settings(server)
        }),
        ServerKind::Shadowsocks => json!({
            "tag": "proxy",
            "protocol": "shadowsocks",
            "settings": {
                "servers": [{
                    "address": server.address,
                    "port": server.port,
                    "method": server.method.clone().unwrap_or_default(),
                    "password": server.uuid,
                    "level": 8,
                    "email": "ss@rprx"
                }]
            },
            "streamSettings": {"network":"tcp","sockopt":{"tcpNoDelay":true}}
        }),
        ServerKind::Hysteria2 => json!({
            // xray >=26: protocol "hysteria", version 2, auth в hysteriaSettings
            "tag": "proxy",
            "protocol": "hysteria",
            "settings": {
                "version": 2,
                "address": server.address,
                "port": server.port
            },
            "streamSettings": {
                "network": "hysteria",
                "security": "tls",
                "tlsSettings": {
                    "serverName": server.sni.clone().unwrap_or_else(|| server.address.clone()),
                    "alpn": ["h3"],
                    "allowInsecure": server.insecure
                },
                "hysteriaSettings": {
                    "version": 2,
                    "auth": server.uuid
                },
                // провайдерский obfs salamander приезжает параметром fm (JSON xray)
                "finalmask": server
                    .finalmask
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
            }
        }),
    }
}

/// Конфиг sing-box в точности по образцу Happ TTunConfigurator (см.
/// docs/RESEARCH-happ-4.7.2.md): TUN c auto_route/strict_route поверх SOCKS xray,
/// собственные процессы ядра — direct, DNS перехватывается внутри туннеля.
pub fn build_singbox(st: &Settings, iface: &str) -> Value {
    let mut addr = vec![st.tun_address.clone()];
    // Happ добавляет IPv6-адрес только когда он задан; у нас префикса ULA нет — не добавляем
    json!({
        "log": { "level": "info", "timestamp": true },
        "dns": { "servers": [
            { "type": "udp", "tag": "dns-proxy",
              "server": st.dns.first().cloned().unwrap_or_else(|| "8.8.8.8".into()),
              "detour": "proxy" }
        ]},
        "inbounds": [
            { "type": "tun", "tag": "tun-in", "interface_name": iface,
              "address": addr, "mtu": 1500,
              "auto_route": true, "strict_route": true, "stack": "mixed" }
        ],
        "outbounds": [
            { "type": "socks", "tag": "proxy", "server": "127.0.0.1",
              "server_port": st.socks_port, "udp_fragment": true,
              "domain_resolver": { "type": "server", "server": "dns-proxy" } },
            { "type": "direct", "tag": "direct",
              "domain_resolver": { "type": "server", "server": "dns-proxy" } }
        ],
        "route": {
            "final": "proxy",
            "auto_detect_interface": true,
            "rules": [
                { "process_name": if st.direct_apps.is_empty() {
                        vec!["omaxy".to_string(), "xray".to_string(),
                             "sing-box".to_string(), "hermes".to_string()]
                    } else { st.direct_apps.clone() },
                  "outbound": "direct" },
                // весь Hermes и его бэкенды (python/всё из ~/.hermes) — мимо туннеля,
                // связь с моделью не должна зависеть от состояния прокси
                { "process_path_regex": format!(
                    "^{}/\\.hermes/",
                    std::env::var("HOME").unwrap_or_else(|_| "/home/mikkato".into())),
                  "outbound": "direct" },
                { "action": "sniff" },
                { "protocol": "dns", "action": "hijack-dns" }
            ]
        }
    })
}

/// путь к sing-box: настройки → ~/.local/share/omaxy → /usr/bin
pub fn resolve_singbox(st: &Settings) -> Option<String> {
    let mut cands: Vec<String> = Vec::new();
    if !st.singbox_path.is_empty() {
        cands.push(st.singbox_path.clone());
    }
    if let Some(home) = std::env::var_os("HOME") {
        cands.push(format!("{}/.local/share/omaxy/sing-box", home.to_string_lossy()));
    }
    cands.push("/usr/bin/sing-box".into());
    cands.into_iter().find(|p| std::path::Path::new(p).exists())
}

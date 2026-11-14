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
                "address": [server_tun_address(st)],
                "gateway": st.tun_gateway,
                "mtu": 9000,
                "strictRoute": false
            }
        }));
    }

    let mut rules: Vec<Value> = Vec::new();
    if with_tun {
        rules.push(json!({"type":"field","inboundTag":["tun-in"],"outboundTag":"dns-out"}));
        rules.push(json!({"type":"field","network":"udp","port":"53","outboundTag":"dns-out"}));
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
                    "id": server.uuid,
                    "encryption": server.encryption.clone().unwrap_or_else(|| "none".into()),
                    "flow": server.flow.clone().unwrap_or_default(),
                    "level": 8
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
        ServerKind::Hysteria2 => {
            let mut ss = stream_settings(server);
            if let Some(obj) = ss.as_object_mut() {
                obj.insert("network".into(), json!("hysteria2"));
                let mut h2 = Map::new();
                h2.insert("password".into(), json!(server.uuid));
                h2.insert("disableMtuProbe".into(), json!(false));
                obj.insert("hysteria2Settings".into(), Value::Object(h2));
            }
            json!({
                "tag": "proxy",
                "protocol": "freedom",
                "settings": {"domainStrategy": "UseIP", "redirect": ""},
                "streamSettings": ss
            })
        }
    }
}

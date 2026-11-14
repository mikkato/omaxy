//! Parsing of proxy share links into a neutral Server model.
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServerKind {
    Vless,
    Vmess,
    Trojan,
    Shadowsocks,
    Hysteria2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub kind: ServerKind,
    pub address: String,
    pub port: u16,
    pub uuid: String,        // uuid / password / method:password
    pub method: Option<String>, // ss method
    pub flow: Option<String>,
    pub encryption: Option<String>,
    pub network: Option<String>, // tcp|grpc|ws|kcp|h2|xhttp
    pub security: Option<String>, // tls|reality|none
    pub sni: Option<String>,
    pub pbk: Option<String>,
    pub sid: Option<String>,
    pub fp: Option<String>,
    pub public_key: Option<String>, // hysteria2
    pub pepper: Option<String>,   // hysteria2 obfs (salamander) password
    pub finalmask: Option<String>, // hysteria2 "fm" JSON (xray finalmask/salamander)
    pub upmbps: Option<String>,
    pub downmbps: Option<String>,
    pub insecure: bool,
    pub service_name: Option<String>, // grpc
    pub path: Option<String>,
    pub host: Option<String>,
    pub headers: serde_json::Map<String, serde_json::Value>,
    pub raw: String,
}

fn q(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string())
}

fn gen_id(raw: &str) -> String {
    // short stable hash (FNV-1a over raw uri)
    let mut h: u64 = 0xcbf29ce484222325;
    for b in raw.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

pub fn from_uri(uri: &str) -> Result<Server, String> {
    let uri = uri.trim();
    if uri.starts_with("vmess://") {
        return from_vmess(uri);
    }
    let url = Url::parse(uri).map_err(|e| format!("bad uri: {e}"))?;
    let kind = match url.scheme() {
        "vless" => ServerKind::Vless,
        "trojan" => ServerKind::Trojan,
        "ss" => ServerKind::Shadowsocks,
        "hysteria2" | "hy2" => ServerKind::Hysteria2,
        s => return Err(format!("unsupported scheme: {s}")),
    };
    let name = url
        .fragment()
        .map(|f| percent_decode(f))
        .unwrap_or_else(|| format!("{}:{}", url.host_str().unwrap_or("?"), url.port().unwrap_or(0)));
    let address = url
        .host_str()
        .ok_or_else(|| "missing host".to_string())?
        .to_string();
    let port = url.port().ok_or_else(|| "missing port".to_string())?;
    let uuid = percent_decode(url.username());
    let mut s = Server {
        id: gen_id(uri),
        name,
        kind: kind.clone(),
        address,
        port,
        uuid,
        method: None,
        flow: q(&url, "flow"),
        encryption: q(&url, "encryption"),
        network: q(&url, "type").or_else(|| q(&url, "network")),
        security: q(&url, "security"),
        sni: q(&url, "sni").or_else(|| q(&url, "peer")),
        pbk: q(&url, "pbk"),
        sid: q(&url, "sid"),
        fp: q(&url, "fp"),
        public_key: q(&url, "pubkey").or_else(|| q(&url, "pk")),
        pepper: q(&url, "pepper").or_else(|| q(&url, "obfs-password")),
        finalmask: q(&url, "fm"),
        upmbps: q(&url, "upmbps").or_else(|| q(&url, "UP")),
        downmbps: q(&url, "downmbps").or_else(|| q(&url, "DP")),
        insecure: q(&url, "insecure").as_deref() == Some("1"),
        service_name: q(&url, "serviceName").or_else(|| q(&url, "mode")),
        path: q(&url, "path").or_else(|| q(&url, "serviceName")),
        host: q(&url, "host"),
        headers: serde_json::Map::new(),
        raw: uri.to_string(),
    };
    if s.security.is_none() {
        s.security = Some("none".into());
    }
    if kind == ServerKind::Shadowsocks {
        // ss://base64(method:password)@host:port or ss://method:password@host:port
        let userinfo = percent_decode(url.username());
        let decoded = STANDARD
            .decode(&userinfo)
            .or_else(|_| STANDARD_NO_PAD.decode(&userinfo))
            .ok()
            .and_then(|d| String::from_utf8(d).ok())
            .unwrap_or(userinfo.clone());
        if decoded.contains(':') {
            let (m, p) = decoded.split_once(':').unwrap();
            s.method = Some(m.to_string());
            s.uuid = p.to_string();
        } else {
            return Err("shadowsocks: cannot parse method:password".into());
        }
    }
    if kind == ServerKind::Vless && s.encryption.as_deref().unwrap_or("none").is_empty() {
        s.encryption = Some("none".into());
    }
    Ok(s)
}

fn from_vmess(uri: &str) -> Result<Server, String> {
    let b64 = uri.trim_start_matches("vmess://");
    let json: serde_json::Value = STANDARD
        .decode(b64)
        .or_else(|_| STANDARD_NO_PAD.decode(b64))
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .ok_or("vmess: bad base64 json")?;
    let g = |k: &str| json.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
    let port = json
        .get("port")
        .map(|v| match v {
            serde_json::Value::String(s) => s.parse::<u16>().unwrap_or(0),
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) as u16,
            _ => 0,
        })
        .unwrap_or(0);
    let net = g("net");
    let security = g("tls").map(|t| if t == "tls" { "tls".into() } else { "none".into() })
        .or_else(|| g("security").map(|s| if s == "tls" || s == "reality" { s } else { "none".into() }));
    let mut headers = serde_json::Map::new();
    if let Some(h) = json.get("host").and_then(|v| v.as_str()) {
        if !h.is_empty() {
            headers.insert("Host".into(), h.into());
        }
    }
    Ok(Server {
        id: gen_id(uri),
        name: g("ps").unwrap_or_else(|| format!("{}:{}", g("add").unwrap_or_default(), port)),
        kind: ServerKind::Vmess,
        address: g("add").ok_or("vmess: missing add")?,
        port,
        uuid: g("id").ok_or("vmess: missing id")?,
        method: Some(g("cipher").unwrap_or_else(|| "auto".into())),
        flow: g("flow"),
        encryption: None,
        network: Some(net.unwrap_or_else(|| "tcp".into())),
        security: Some(security.unwrap_or_else(|| "none".into())),
        sni: g("sni"),
        pbk: g("pbk"),
        sid: g("sid"),
        fp: g("fp"),
        public_key: None,
        pepper: None,
        finalmask: None,
        upmbps: None,
        downmbps: None,
        insecure: g("allowInsecure").as_deref() == Some("1"),
        service_name: if g("type").is_some() { g("path") } else { None },
        path: g("path"),
        host: g("host"),
        headers,
        raw: uri.to_string(),
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vless_reality() {
        let s = from_uri(
            "vless://11111111-2222-3333-4444-555555555555@example.com:443?encryption=none&security=reality&type=tcp&flow=xtls-rprx-vision&sni=www.microsoft.com&pbk=abc&sid=6ba85179&fp=chrome#Test%20Server",
        )
        .unwrap();
        assert_eq!(s.kind, ServerKind::Vless);
        assert_eq!(s.port, 443);
        assert_eq!(s.security.as_deref(), Some("reality"));
        assert_eq!(s.flow.as_deref(), Some("xtls-rprx-vision"));
        assert_eq!(s.name, "Test Server");
    }

    #[test]
    fn ss_basic() {
        let enc = STANDARD.encode("chacha20-ietf-poly1305:secret");
        let s = from_uri(&format!("ss://{enc}@1.2.3.4:8388#ss1")).unwrap();
        assert_eq!(s.method.as_deref(), Some("chacha20-ietf-poly1305"));
        assert_eq!(s.uuid, "secret");
    }
}

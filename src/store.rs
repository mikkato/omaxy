//! User config store: ~/.config/omaxy/config.json
use crate::links::Server;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub servers: Vec<Server>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub socks_port: u16,
    pub http_port: u16,
    pub dns: Vec<String>,
    /// "direct-local": LAN/私网 direct, остальное через прокси. "global": всё через прокси.
    pub route_mode: String,
    pub xray_path: String,
    pub tun_address: String,
    pub tun_gateway: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            socks_port: 10808,
            http_port: 10809,
            dns: vec!["1.1.1.1".into(), "9.9.9.9".into()],
            route_mode: "direct-local".into(),
            xray_path: "/usr/bin/xray".into(),
            tun_address: "10.259.260.1/30".into(),
            tun_gateway: "10.259.260.2".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub servers: Vec<Server>,
    pub subscriptions: Vec<Subscription>,
    pub settings: Settings,
    pub active: Option<String>, // server id
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config"));
    base.join("omaxy")
}

pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/state"));
    base.join("omaxy")
}

pub fn load() -> Config {
    let path = config_dir().join("config.json");
    match fs::read_to_string(&path) {
        Ok(s) => {
            let mut cfg: Config = serde_json::from_str(&s).unwrap_or_default();
            if cfg.settings.dns.is_empty() {
                cfg.settings = Settings::default();
            }
            cfg
        }
        Err(_) => Config {
            settings: Settings::default(),
            ..Default::default()
        },
    }
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(dir.join("config.json"), json).map_err(|e| e.to_string())
}

impl Config {
    pub fn find_server(&self, sel: &str) -> Option<&Server> {
        self.all_servers()
            .into_iter()
            .find(|s| s.id == sel || s.name == sel || s.id.starts_with(sel))
    }

    pub fn all_servers(&self) -> Vec<&Server> {
        let mut v: Vec<&Server> = self.servers.iter().collect();
        for sub in &self.subscriptions {
            v.extend(sub.servers.iter());
        }
        v
    }
}

/// Stable device HWID, Happ-style: 16 lowercase hex chars from the machine id
/// (sha256 truncated). Panels (Remnawave/Subacone & co) gate subscriptions on
/// the x-hwid header and count devices by it.
pub fn hwid() -> String {
    let mi = ["/var/lib/dbus/machine-id", "/etc/machine-id"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| "omaxy-fallback-device".to_string());
    // hash raw file bytes (incl. trailing \n) to match `sha256sum machine-id`
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(mi.as_bytes());
    h.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Fetch a subscription URL: link list (base64 or plaintext) or JSON array.
pub fn fetch_subscription(url: &str) -> Result<Vec<Server>, String> {
    let resp = ureq::get(url)
        .set("User-Agent", "Happ/4.7.2") // Happ-family panels gate by UA + HWID headers
        .set("x-hwid", &hwid())
        .set("x-device-os", "Linux")
        .set("x-device-model", "omaxy")
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    let hwid_flag = resp
        .header("x-hwid-not-supported")
        .unwrap_or("")
        .to_string();
    let body = resp.into_string().map_err(|e| e.to_string())?;
    let mut uris: Vec<String> = Vec::new();
    let trimmed = body.trim();
    let candidates: Vec<String> = if trimmed.starts_with('[') {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(trimmed).map_err(|e| e.to_string())?;
        arr.into_iter()
            .filter_map(|v| {
                v.as_str().map(|s| s.to_string()).or_else(|| {
                    v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string())
                })
            })
            .collect()
    } else {
        let mut c = vec![trimmed.to_string()];
        // maybe base64 blob of newline-separated uris
        if let Ok(d) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, trimmed.replace('\n', "")) {
            if let Ok(s) = String::from_utf8(d) {
                c.push(s);
            }
        }
        c
    };
    for c in &candidates {
        for line in c.lines() {
            let l = line.trim();
            if l.contains("://") {
                uris.push(l.to_string());
            }
        }
    }
    uris.dedup();
    let mut servers = Vec::new();
    for u in uris {
        if let Ok(s) = crate::links::from_uri(&u) {
            // HWID-gate decoys from Happ-family panels: 0.0.0.0:1 with hint names
            if s.address == "0.0.0.0" {
                continue;
            }
            servers.push(s);
        }
    }
    if servers.is_empty() {
        if body.contains("HWID") {
            return Err(if hwid_flag == "true" {
                "панель не принимает HWID от этого клиента (список приложений ограничен)".into()
            } else {
                "панель требует включить HWID на подписке (проверьте бота/кабинет провайдера)".into()
            });
        }
        return Err("subscription: no parsable links found".into());
    }
    Ok(servers)
}

fn break_after_first_parse(_uris: &mut Vec<String>, _all: &[String]) {}

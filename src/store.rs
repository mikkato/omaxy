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

/// Fetch a subscription URL: link list (base64 or plaintext) or JSON array.
pub fn fetch_subscription(url: &str) -> Result<Vec<Server>, String> {
    let resp = ureq::get(url)
        .set("User-Agent", "clash-verge/1.0") // many panels gate by UA
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
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
            servers.push(s);
        }
    }
    if servers.is_empty() {
        return Err("subscription: no parsable links found".into());
    }
    Ok(servers)
}

fn break_after_first_parse(_uris: &mut Vec<String>, _all: &[String]) {}

//! omaxy — CLI client + daemon entry. Wire-compatible with happd.
mod codec;
mod daemon;
mod links;
mod store;
mod xrayconf;

use codec::{read_frame, write_frame};
use serde_json::{json, Value};
use std::io::BufReader;
use std::os::unix::net::UnixStream;

fn socket_path() -> String {
    std::env::var("OMAXY_SOCK").unwrap_or_else(|_| "/tmp/happd.sock".to_string())
}

struct Conn {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Conn {
    fn open() -> Result<Conn, String> {
        let stream = UnixStream::connect(socket_path())
            .map_err(|_| "omaxyd не запущен (systemctl status omaxyd)".to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .ok();
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| e.to_string())?,
        );
        let mut c = Conn { stream, reader };
        let hello = c.call(json!({
            "action": "client-hello",
            "client-version": "omaxy-cli/1.0.0",
            "request-id": 1
        }))?;
        if !hello.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Err("client-hello rejected".into());
        }
        Ok(c)
    }

    fn call(&mut self, msg: Value) -> Result<Value, String> {
        write_frame(&mut self.stream, &msg).map_err(|e| e.to_string())?;
        read_frame(&mut self.reader)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "daemon closed connection".to_string())
    }
}

fn cli_call(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: omaxy call <action> ['{json}']".into());
    }
    let mut msg: Value = json!({"action": args[0], "request-id": 100});
    if let Some(extra) = args.get(1) {
        let e: Value = serde_json::from_str(extra).map_err(|e| e.to_string())?;
        if let (Some(o), Some(eo)) = (msg.as_object_mut(), e.as_object()) {
            for (k, v) in eo {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    let mut c = Conn::open()?;
    let r = c.call(msg)?;
    println!("{}", serde_json::to_string_pretty(&r).unwrap());
    Ok(())
}

fn write_active(info: &Value) -> Result<(), String> {
    let dir = store::state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("active.json"), serde_json::to_string_pretty(info).unwrap())
        .map_err(|e| e.to_string())
}

fn read_active() -> Option<Value> {
    std::fs::read_to_string(store::state_dir().join("active.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let res: Result<(), String> = match cmd {
        "daemon" => {
            let d = daemon::Daemon::new(socket_path());
            d.run().map_err(|e| format!("daemon: {e}"))
        }
        "server" => cli_server(&args[1..]),
        "subscribe" => cli_subscribe(&args[1..]),
        "connect" => cli_connect(&args[1..]),
        "disconnect" => cli_disconnect(),
        "status" => cli_status(&args[1..]),
        "qr" => cli_qr(&args[1..]),
        "call" => cli_call(&args[1..]),
        _ => {
            println!(
                "omaxy — open Xray client for Omarchy (happd-wire-compatible daemon)

usage:
  omaxy daemon                     run the daemon (root, systemd unit)
  omaxy server add <uri> [name]    add server from vless/vmess/trojan/ss/hy2 link
  omaxy server list|rm <sel>
  omaxy subscribe add <url> [name] fetch & store subscription links
  omaxy subscribe update [sel]
  omaxy connect [server]           start xray (TUN + socks/http) via daemon
  omaxy disconnect
  omaxy status [--json]
  omaxy qr <file.png>              decode a proxy link from a QR image
  omaxy call <action> [json]       raw daemon call"
            );
            Ok(())
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn cli_server(args: &[String]) -> Result<(), String> {
    let mut cfg = store::load();
    match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "add" => {
            let uri = args.get(1).ok_or("usage: omaxy server add <uri> [name]")?;
            let mut s = links::from_uri(uri)?;
            if let Some(name) = args.get(2) {
                s.name = name.clone();
            }
            if cfg.all_servers().iter().any(|x| x.raw == s.raw) {
                return Err("server already exists".into());
            }
            println!("added: {} ({:?})", s.name, s.kind);
            cfg.servers.push(s);
            store::save(&cfg)
        }
        "list" => {
            let all = cfg.all_servers();
            if all.is_empty() {
                println!("нет серверов — добавь: omaxy server add <uri>");
            }
            for s in all {
                let active_mark = if cfg.active.as_deref() == Some(s.id.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!("{active_mark} {:<38} {} {}:{} {}", truncate(&s.name, 38), proto(s), s.address, s.port, &s.id[..8]);
            }
            Ok(())
        }
        "rm" => {
            let sel = args.get(1).ok_or("usage: omaxy server rm <sel>")?;
            let before = cfg.servers.len();
            cfg.servers.retain(|s| !(s.id == *sel || s.name == *sel || s.id.starts_with(sel)));
            if cfg.servers.len() == before {
                return Err("not found".into());
            }
            store::save(&cfg)
        }
        _ => Err("usage: omaxy server add|list|rm".into()),
    }
}

fn proto(s: &links::Server) -> &'static str {
    use links::ServerKind::*;
    match s.kind {
        Vless => "vless",
        Vmess => "vmess",
        Trojan => "trojan",
        Shadowsocks => "ss",
        Hysteria2 => "hy2",
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

fn cli_subscribe(args: &[String]) -> Result<(), String> {
    let mut cfg = store::load();
    match args.first().map(|s| s.as_str()).unwrap_or("") {
        "add" => {
            let url = args.get(1).ok_or("usage: omaxy subscribe add <url> [name]")?;
            let servers = store::fetch_subscription(url)?;
            let name = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| url.split('/').next_back().unwrap_or("sub").to_string());
            let id = format!("sub-{}", servers.len() + cfg.subscriptions.len());
            println!("подписка «{name}»: {} серверов", servers.len());
            cfg.subscriptions.push(store::Subscription {
                id,
                name,
                url: url.clone(),
                updated_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                servers,
            });
            store::save(&cfg)
        }
        "update" => {
            let sel = args.get(1);
            let mut n = 0;
            for sub in cfg.subscriptions.iter_mut() {
                if sel.is_none() || Some(&sub.id) == sel || Some(&sub.name) == sel {
                    match store::fetch_subscription(&sub.url) {
                        Ok(servers) => {
                            println!("{}: {} серверов", sub.name, servers.len());
                            sub.servers = servers;
                            sub.updated_at = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();
                            n += 1;
                        }
                        Err(e) => eprintln!("{}: {e}", sub.name),
                    }
                }
            }
            if n == 0 {
                return Err("nothing updated".into());
            }
            store::save(&cfg)
        }
        _ => Err("usage: omaxy subscribe add <url> [name] | update [sel]".into()),
    }
}

fn cli_connect(args: &[String]) -> Result<(), String> {
    let cfg = store::load();
    let sel = args.first().cloned().or_else(|| cfg.active.clone());
    let server = match &sel {
        Some(s) => cfg.find_server(s).cloned(),
        None => cfg.all_servers().first().map(|s| (*s).clone()),
    }
    .ok_or("сервер не найден; список: omaxy server list")?;
    if server.kind == links::ServerKind::Hysteria2 {
        return Err("hysteria2 в генераторе xray-конфига пока не поддерживается".into());
    }
    let conf = xrayconf::build(&server, &cfg.settings, true);
    let conf_str = serde_json::to_string_pretty(&conf).unwrap();
    let mut c = Conn::open()?;
    let put = c.call(json!({
        "action": "omaxy-put", "path": "xray-active.json", "content": conf_str, "request-id": 2
    }))?;
    if !put.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(format!("put failed: {put}"));
    }
    let start = c.call(json!({
        "action": "start",
        "process-id": "omaxy-xray",
        "executable": cfg.settings.xray_path,
        "arguments": ["run", "-c", "/var/lib/omaxy/xray-active.json"],
        "environment": {"OMAXY_TUN": "1"},
        "auto-restart": true,
        "max-restart-attempts": 5,
        "restart-delay-ms": 3000,
        "detached": true,
        "request-id": 3
    }))?;
    if !start.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(format!("start failed: {start}"));
    }
    let mut cfg2 = cfg.clone();
    cfg2.active = Some(server.id.clone());
    store::save(&cfg2)?;
    write_active(&json!({
        "server_id": server.id, "name": server.name, "proto": proto(&server),
        "since": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }))?;
    println!("connected: {} ({}:{})", server.name, server.address, server.port);
    Ok(())
}

fn cli_disconnect() -> Result<(), String> {
    let mut c = Conn::open()?;
    let r = c.call(json!({
        "action": "stop", "process-id": "omaxy-xray", "request-id": 4
    }))?;
    let _ = std::fs::remove_file(store::state_dir().join("active.json"));
    let mut cfg = store::load();
    cfg.active = None;
    store::save(&cfg)?;
    println!("{}", r.get("message").and_then(|v| v.as_str()).unwrap_or("disconnected"));
    Ok(())
}

fn cli_status(args: &[String]) -> Result<(), String> {
    let json_out = args.iter().any(|a| a == "--json");
    let cfg = store::load();
    let servers: Vec<Value> = cfg
        .all_servers()
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "proto": proto(s),
                "address": s.address, "port": s.port,
                "active": cfg.active.as_deref() == Some(s.id.as_str())
            })
        })
        .collect();
    let mut out = json!({"daemon": false, "connected": false, "servers": servers,
                         "subscriptions": cfg.subscriptions.iter().map(|x| json!({"id":x.id,"name":x.name,"count":x.servers.len()})).collect::<Vec<_>>()});
    if let Ok(mut c) = Conn::open() {
        if let Ok(r) = c.call(json!({"action": "omaxy-status", "request-id": 5})) {
            let running = r
                .get("processes")
                .and_then(|p| p.as_array())
                .map(|list| {
                    list.iter().any(|p| {
                        p.get("process-id").and_then(|v| v.as_str()) == Some("omaxy-xray")
                            && p.get("state").and_then(|v| v.as_str()) == Some("started")
                    })
                })
                .unwrap_or(false);
            out = json!({
                "daemon": true,
                "daemon_version": r.get("daemon-version"),
                "connected": running,
                "active": read_active(),
                "servers": servers,
                "subscriptions": out["subscriptions"].clone(),
                "processes": r.get("processes").cloned().unwrap_or(json!([]))
            });
        }
    }
    if json_out {
        println!("{}", serde_json::to_string(&out).unwrap());
    } else {
        if !out["daemon"].as_bool().unwrap_or(false) {
            println!("демон не отвечает ({})", socket_path());
            return Ok(());
        }
        if out["connected"].as_bool().unwrap_or(false) {
            let a = &out["active"];
            println!(
                "⬤ подключено: {} ({}), с {}",
                a["name"].as_str().unwrap_or("?"),
                a["proto"].as_str().unwrap_or("?"),
                a["since"]
                    .as_u64()
                    .map(|t| {
                        let secs = t;
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let d = now.saturating_sub(secs);
                        format!("{}м{}с назад", d / 60, d % 60)
                    })
                    .unwrap_or_default()
            );
        } else {
            println!("○ не подключено");
        }
    }
    Ok(())
}

fn cli_qr(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("usage: omaxy qr <file.png>")?;
    let img = image::open(path).map_err(|e| e.to_string())?.to_luma8();
    let mut img = rqrr::PreparedImage::prepare(img);
    let grids = img.detect_grids();
    for g in grids {
        if let Ok((_meta, text)) = g.decode() {
            println!("{text}");
            return Ok(());
        }
    }
    Err("QR не найден".into())
}

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

/// Cumulative TUN byte counters from sysfs (iface set in xrayconf: omaxy0).
fn read_traffic() -> Option<Value> {
    for iface in ["omaxy0", "tun0"] {
        let rd = |f: &str| {
            std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{f}"))
                .ok()?
                .trim()
                .parse::<u64>()
                .ok()
        };
        if let (Some(rx), Some(tx)) = (rd("rx_bytes"), rd("tx_bytes")) {
            return Some(json!({"iface": iface, "rx": rx, "tx": tx}));
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let res: Result<(), String> = match cmd {
        "daemon" => {
            if args.get(1).map(|s| s.as_str()) == Some("ensure") {
                cli_daemon_ensure()
            } else {
                let daemonize = args.iter().any(|a| a == "--daemonize");
                if daemonize {
                    if let Err(e) = daemonize_into_log() {
                        eprintln!("daemonize: {e}");
                        std::process::exit(1);
                    }
                }
                let d = daemon::Daemon::new(socket_path());
                d.run().map_err(|e| format!("daemon: {e}"))
            }
        }
        "server" => cli_server(&args[1..]),
        "subscribe" => cli_subscribe(&args[1..]),
        "connect" => cli_connect(&args[1..]),
        "disconnect" => cli_disconnect(),
        "status" => cli_status(&args[1..]),
        "qr" => cli_qr(&args[1..]),
        "ping" => cli_ping(&args[1..]),
        "config" => cli_config(&args[1..]),
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
    let conf = xrayconf::build(&server, &cfg.settings, cfg.settings.tun);
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
                         "settings": serde_json::to_value(&cfg.settings).unwrap_or(json!({})),
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
                "traffic": read_traffic(),
                "settings": serde_json::to_value(&cfg.settings).unwrap_or(json!({})),
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

/// TCP connect latency to the server endpoint (ICMP rarely works on proxy ports):
/// 3 attempts, min/median in ms.
fn cli_ping(args: &[String]) -> Result<(), String> {
    let json_out = args.iter().any(|a| a == "--json");
    let sel = args.iter().find(|a| !a.starts_with("--")).cloned();
    let cfg = store::load();
    let server = match &sel {
        Some(s) => cfg.find_server(s).cloned(),
        None => cfg.active.as_ref().and_then(|a| cfg.find_server(a).cloned()),
    }
    .ok_or("сервер не найден; список: omaxy server list")?;
    let target = format!("{}:{}", server.address, server.port);
    let addrs: Vec<std::net::SocketAddr> = {
        use std::net::ToSocketAddrs;
        target.to_socket_addrs().map_err(|e| format!("{target}: {e}"))?.collect()
    };
    let addrs = addrs.first().ok_or(format!("dns: {target}"))?.to_owned();
    let mut times: Vec<u128> = Vec::new();
    let mut last_err = String::new();
    for _ in 0..3 {
        let t0 = std::time::Instant::now();
        match std::net::TcpStream::connect_timeout(
            &addrs,
            std::time::Duration::from_millis(3000),
        ) {
            Ok(_) => times.push(t0.elapsed().as_millis()),
            Err(e) => last_err = e.to_string(),
        }
    }
    if times.is_empty() {
        let msg = format!("{target}: {last_err}");
        if json_out {
            println!("{}", json!({"id": server.id, "ok": false, "error": msg}));
            return Ok(());
        }
        return Err(msg);
    }
    times.sort_unstable();
    let out = json!({
        "id": server.id, "ok": true, "target": target,
        "min_ms": times[0], "median_ms": times[times.len()/2], "attempts": times
    });
    if json_out {
        println!("{out}");
    } else {
        println!("{target}: {} мс (мин из {})", times[0], times.len());
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

/// Double-fork detach so pkexec/systemd-run can return while the daemon lives on.
fn daemonize_into_log() -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err("fork".into());
        }
        if pid > 0 {
            // first parent (pkexec's child) returns immediately; daemon lives in the child
            std::process::exit(0);
        }
    }
    // We are the child: new session, second fork to drop session leadership.
    unsafe { libc::setsid() };
    unsafe {
        if libc::fork() < 0 {
            std::process::exit(1);
        }
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/var/log/omaxyd.log")
        .or_else(|_| OpenOptions::new().create(true).append(true).open("/tmp/omaxyd.log"))
        .map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let devnull = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
    unsafe {
        libc::dup2(devnull.as_raw_fd(), 0);
        libc::dup2(log.as_raw_fd(), 1);
        libc::dup2(log_err.as_raw_fd(), 2);
    }
    Ok(())
}

/// Start the daemon on demand: systemd unit if installed, else pkexec self-spawn.
fn cli_daemon_ensure() -> Result<(), String> {
    if Conn::open().is_ok() {
        println!("демон уже работает");
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    // 1) установленный unit (unit uses OMAXY_SOCK; probe it before claiming failure)
    let unit = std::process::Command::new("systemctl")
        .args(["cat", "omaxyd.service"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if unit {
        let st = std::process::Command::new("systemctl")
            .args(["start", "omaxyd.service"])
            .status()
            .map_err(|e| e.to_string())?;
        if st.success() && wait_daemon() {
            println!("демон запущен (omaxyd.service)");
            return Ok(());
        }
        let st = std::process::Command::new("pkexec")
            .args(["systemctl", "start", "omaxyd.service"])
            .status()
            .map_err(|e| format!("pkexec: {e}"))?;
        if st.success() && wait_daemon() {
            println!("демон запущен (omaxyd.service, polkit)");
            return Ok(());
        }
    }
    // 2) unit нет — detach-запуск себя под root через polkit
    let st = std::process::Command::new("pkexec")
        .args([&exe, "daemon", "--daemonize"])
        .status()
        .map_err(|e| format!("pkexec: {e}"))?;
    if !st.success() {
        return Err("не удалось получить root (polkit отклонён?)".into());
    }
    if wait_daemon() {
        println!("демон запущен (pkexec, лог: /var/log/omaxyd.log или /tmp/omaxyd.log)");
        Ok(())
    } else {
        Err("pkexec прошёл, но демон не отвечает".into())
    }
}

fn wait_daemon() -> bool {
    for _ in 0..40 {
        if Conn::open().is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// omaxy config                     — print settings JSON
/// omaxy config set <k> <v> [...]   — set settings; reconnects live xray if active
fn cli_config(args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()).unwrap_or("get") {
        "get" => {
            let cfg = store::load();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::to_value(&cfg.settings).unwrap()).unwrap()
            );
            Ok(())
        }
        "set" => {
            let mut cfg = store::load();
            let pairs = &args[1..];
            if pairs.len() % 2 != 0 {
                return Err("usage: omaxy config set <key> <value> [<key> <value> …]".into());
            }
            let mut changed_keys = Vec::new();
            let mut i = 0;
            while i < pairs.len() {
                let (k, v) = (&pairs[i], pairs[i + 1].as_str());
                i += 2;
                let res: Result<(), String> = match k.as_str() {
                    "socks_port" => v.parse::<u16>().map(|p| cfg.settings.socks_port = p).map_err(|e| e.to_string()),
                    "http_port" => v.parse::<u16>().map(|p| cfg.settings.http_port = p).map_err(|e| e.to_string()),
                    "dns" => {
                        cfg.settings.dns = v
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        Ok(())
                    }
                    "route_mode" => {
                        if v == "direct-local" || v == "global" {
                            cfg.settings.route_mode = v.to_string();
                            Ok(())
                        } else {
                            Err("route_mode: direct-local | global".to_string())
                        }
                    }
                    "tun" => v.parse::<bool>().map(|b| cfg.settings.tun = b).map_err(|e| e.to_string()),
                    "tun_address" => {
                        if store_valid_v4(v.split('/').next().unwrap_or("")) {
                            cfg.settings.tun_address = v.to_string();
                            Ok(())
                        } else {
                            Err("невалидный IPv4/CIDR".to_string())
                        }
                    }
                    "tun_gateway" => {
                        if store_valid_v4(v) {
                            cfg.settings.tun_gateway = v.to_string();
                            Ok(())
                        } else {
                            Err("невалидный IPv4".to_string())
                        }
                    }
                    "xray_path" => {
                        cfg.settings.xray_path = v.to_string();
                        Ok(())
                    }
                    other => Err(format!("неизвестный ключ: {other}")),
                };
                res.map_err(|e| format!("{k}: {e}"))?;
                changed_keys.push(k.clone());
            }
            store::save(&cfg)?;
            println!("сохранено: {}", changed_keys.join(", "));
            // настройки применяются к запущенному xray только перезапуском
            if cfg.active.is_some() && Conn::open().is_ok() {
                println!("перезапуск подключения…");
                cli_disconnect()?;
                cli_connect(&[])?;
            }
            Ok(())
        }
        _ => Err("usage: omaxy config [get | set <k> <v> …]".into()),
    }
}

fn store_valid_v4(s: &str) -> bool {
    !s.is_empty() && s.split('.').count() == 4 && s.split('.').all(|o| o.parse::<u8>().is_ok())
}

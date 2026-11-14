//! omaxyd — root process-control daemon, wire-compatible with happd:
//! unix socket, u32 BE frame + JSON, actions client-hello/heartbeat/start/stop/kill/...
use crate::codec::{read_frame, write_frame};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DAEMON_VERSION: &str = "omaxy/1.0.0";
pub const LIB_DIR: &str = "/var/lib/omaxy";
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct ProcSpec {
    id: String,
    executable: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    auto_restart: bool,
    max_restart_attempts: u32,
    restart_delay_ms: u64,
    owner: u64,
    detached: bool,
}

struct ProcEntry {
    spec: ProcSpec,
    state: String,
    pid: Option<u32>,
    started_at: u64,
    attempts: u32,
    stopping: bool,
    stdin_tx: Option<Sender<Vec<u8>>>,
}

type Clients = Arc<Mutex<HashMap<u64, ClientHandle>>>;
struct ClientHandle {
    tx: Sender<Value>,
    seen: Instant,
    observer: bool,
}

pub struct Daemon {
    socket_path: String,
}

fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Daemon {
    pub fn new(socket_path: String) -> Self {
        Self { socket_path }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let _ = std::fs::remove_file(&self.socket_path);
        std::fs::create_dir_all(LIB_DIR).ok();
        // обход приложений переживает перезапуск демона
        if !bypass_members().is_empty() {
            bypass_apply();
        }
        // при старте убираем osиротевший xray от прошлых жизней (иначе "device busy")
        let _ = std::process::Command::new("pkill")
            .args(["-9", "-f", "xray run -c /var/lib/omaxy/"])
            .status();
        for _ in 0..15 {
            if !std::path::Path::new("/sys/class/net/omaxy0").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let listener = UnixListener::bind(&self.socket_path)?;
        std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o666))?;
        eprintln!("omaxyd listening on {}", self.socket_path);

        let (ev_tx, ev_rx): (Sender<Value>, Receiver<Value>) = channel();
        // Replace the lazy bus with this channel by leaking a clone through OnceLock:
        // simplest correct approach — spawn router reading ev_rx, and hand ev_tx to
        // everything that needs to emit.
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let procs: Arc<Mutex<HashMap<String, ProcEntry>>> = Arc::new(Mutex::new(HashMap::new()));

        {
            let clients = clients.clone();
            let procs = procs.clone();
            std::thread::spawn(move || {
                for ev in ev_rx {
                    let pid_key = ev
                        .get("process-id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let owner = {
                        let p = procs.lock().unwrap();
                        p.get(&pid_key).map(|e| e.spec.owner)
                    };
                    if let Some(owner) = owner {
                        let c = clients.lock().unwrap();
                        if let Some(h) = c.get(&owner) {
                            let _ = h.tx.send(ev);
                        }
                    }
                }
            });
        }

        // heartbeat sweeper
        {
            let clients = clients.clone();
            let procs = procs.clone();
            let ev_tx = ev_tx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(10));
                let dead: Vec<u64> = {
                    let mut c = clients.lock().unwrap();
                    let dead: Vec<u64> = c
                        .iter()
                        .filter(|(_, h)| h.seen.elapsed() > HEARTBEAT_TIMEOUT)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in &dead {
                        c.remove(id);
                    }
                    dead
                };
                if dead.is_empty() {
                    continue;
                }
                let mut p = procs.lock().unwrap();
                for (_k, e) in p.iter_mut() {
                    if dead.contains(&e.spec.owner) && !e.stopping {
                        e.stopping = true;
                        if let Some(pid) = e.pid {
                            eprintln!("heartbeat timeout: killing owned process {}", e.spec.id);
                            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                        }
                    }
                }
                let _ = &ev_tx;
            });
        }

        let bus = GlobalBus { tx: ev_tx, procs: procs.clone() };

        let next_client: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    let id = next_client.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let uid = peer_uid(&stream);
                    let clients = clients.clone();
                    let procs = procs.clone();
                    let bus = bus.clone();
                    std::thread::spawn(move || {
                        handle_client(id, uid, stream, clients, procs, bus);
                    });
                }
                Err(e) => eprintln!("accept: {e}"),
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct GlobalBus {
    tx: Sender<Value>,
    procs: Arc<Mutex<HashMap<String, ProcEntry>>>,
}

fn peer_uid(stream: &UnixStream) -> u32 {
    use std::os::unix::io::AsRawFd;
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as u32;
        let rc = libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        );
        if rc == 0 {
            cred.uid
        } else {
            u32::MAX
        }
    }
}

fn handle_client(id: u64, uid: u32, stream: UnixStream, clients: Clients, procs: Arc<Mutex<HashMap<String, ProcEntry>>>, bus: GlobalBus) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(stream));
    let (tx, rx) = channel::<Value>();
    let w2 = writer.clone();
    std::thread::spawn(move || {
        for msg in rx.iter().take_while(|m| {
            let done = write_frame(&mut *w2.lock().unwrap(), m).is_err();
            !done
        }) {
            let _ = msg;
        }
    });
    clients.lock().unwrap().insert(
        id,
        ClientHandle {
            tx: tx.clone(),
            seen: Instant::now(),
            observer: false,
        },
    );
    eprintln!("client connected: {id} (uid {uid})");

    while let Ok(Some(msg)) = read_frame(&mut reader) {
        if let Some(h) = clients.lock().unwrap().get_mut(&id) {
            h.seen = Instant::now();
        }
        let action = msg.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let req_id = msg.get("request-id").cloned().unwrap_or(Value::Null);
        let resp = handle_action(action, &msg, id, uid, &clients, &procs, &bus, &tx);
        if let Some(mut resp) = resp {
            if !req_id.is_null() {
                if let Some(o) = resp.as_object_mut() {
                    o.insert("request-id".into(), req_id);
                }
            }
            if tx.send(resp).is_err() {
                break;
            }
            if action == "shutdown" {
                std::thread::sleep(Duration::from_millis(100));
                std::process::exit(0);
            }
        }
    }
    clients.lock().unwrap().remove(&id);
    // session teardown: kill processes owned by the departed client
    let mut procs = procs.lock().unwrap();
    for (_k, p) in procs.iter_mut() {
        if p.spec.owner == id && !p.stopping {
            p.stopping = true;
            if let Some(pid) = p.pid {
                eprintln!("teardown: killing owned process {}", p.spec.id);
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }
    eprintln!("client disconnected: {id}");
}

fn ok(action: &str, msg: &str) -> Value {
    json!({"success": true, "action": action, "message": msg})
}
fn err(action: &str, msg: &str) -> Value {
    json!({"success": false, "action": action, "message": msg})
}

fn handle_action(
    action: &str,
    msg: &Value,
    id: u64,
    uid: u32,
    clients: &Clients,
    procs: &Arc<Mutex<HashMap<String, ProcEntry>>>,
    bus: &GlobalBus,
    tx: &Sender<Value>,
) -> Option<Value> {
    let _ = tx;
    match action {
        "client-hello" => {
            if let Some(h) = clients.lock().unwrap().get_mut(&id) {
                h.observer = msg.get("observer").and_then(|v| v.as_bool()).unwrap_or(false);
            }
            Some(json!({
                "success": true, "action": "client-hello", "message": "ok",
                "daemon-version": DAEMON_VERSION,
                "privilege-level": if uid == 0 { "root" } else { "user" },
                "client-id": id
            }))
        }
        "heartbeat" => Some(ok("heartbeat", "pong")),
        "get-privilege" => Some(json!({
            "success": true, "action": "get-privilege",
            "privilege-level": if uid == 0 { "root" } else { "user" }
        })),
        "start" => Some(cmd_start(msg, id, procs, bus)),
        "stop" => Some(cmd_stop(msg, procs, false)),
        "kill" => Some(cmd_stop(msg, procs, true)),
        "close-stdin" => {
            let key = msg.get("process-id").and_then(|v| v.as_str()).unwrap_or("");
            let mut procs = procs.lock().unwrap();
            if let Some(p) = procs.get_mut(key) {
                p.stdin_tx = None;
                Some(ok(action, "stdin closed"))
            } else {
                Some(err(action, "Process not found"))
            }
        }
        "reclaim-session-processes" => {
            let mut procs = procs.lock().unwrap();
            let mut n = 0;
            for (_k, p) in procs.iter_mut() {
                if p.spec.owner != id && p.spec.owner != u64::MAX {
                    p.spec.owner = id;
                    n += 1;
                }
            }
            Some(json!({"success": true, "action": action, "reclaimed": n}))
        }
        "omaxy-put" => {
            let name: String = msg
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c))
                .collect();
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return Some(err(action, "bad path"));
            }
            let path = format!("{LIB_DIR}/{name}");
            match std::fs::write(&path, content) {
                Ok(_) => Some(json!({"success": true, "action": action, "path": path})),
                Err(e) => Some(err(action, &e.to_string())),
            }
        }
        "omaxy-status" => {
            let procs = procs.lock().unwrap();
            let list: Vec<Value> = procs
                .values()
                .map(|p| {
                    json!({"process-id": p.spec.id, "state": p.state,
                           "pid": p.pid, "start-time": p.started_at})
                })
                .collect();
            Some(json!({"success": true, "action": action,
                        "processes": list, "daemon-version": DAEMON_VERSION}))
        }
        "clear-logs" => Some(ok(action, "logs cleared")),
                "bypass-add" => {
            let pid = msg.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            match pid_cgroup(pid) {
                None => Some(json!({"success": false, "action": action, "error": "нет такого pid"})),
                Some(src) if src.starts_with("/omaxy-bypass") =>
                    Some(json!({"success": true, "action": action, "moved": 0, "note": "already"})),
                Some(src) => {
                    bypass_apply(); // каталог-приёмник должен существовать до переноса
                    let srcpath = format!("/sys/fs/cgroup{src}");
                    let pre: Vec<u32> = std::fs::read_to_string(format!("{srcpath}/cgroup.procs"))
                        .unwrap_or_default().lines()
                        .filter_map(|l| l.trim().parse::<u32>().ok()).collect();
                    let n = bypass_move_all(&srcpath, BYPASS_CG);
                    {
                        let mut m = bypass_map().lock().unwrap();
                        for p in pre { m.insert(p, src.clone()); }
                    }
                    bypass_map_save();
                    bypass_apply();
                    Some(json!({"success": true, "action": action, "moved": n, "src": src}))
                }
            }
        }
"bypass-remove" => {
            let pid = msg.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let src = {
                let m = bypass_map().lock().unwrap();
                m.get(&pid).cloned().or_else(|| {
                    bypass_members().iter().find_map(|p| m.get(p).cloned())
                })
            };
            match src {
                Some(src) => {
                    let srcpath = format!("/sys/fs/cgroup{src}");
                    let mut n = 0;
                    let back: Vec<u32> = {
                        let mut m = bypass_map().lock().unwrap();
                        let v: Vec<u32> = m.iter().filter(|(_, s)| **s == src)
                            .map(|(p, _)| *p).collect();
                        for p in &v { m.remove(p); }
                        v
                    };
                    for p in back {
                        if std::path::Path::new(&format!("/proc/{p}")).exists()
                            && std::fs::write(format!("{srcpath}/cgroup.procs"), format!("{p}\n")).is_ok() {
                            n += 1;
                        }
                    }
                    bypass_map_save();
                    Some(json!({"success": true, "action": action, "moved": n, "src": src}))
                }
                None => Some(json!({"success": false, "action": action, "error": "не в обходе"})),
            }
        }
        "bypass-list" => Some(json!({
            "success": true, "action": action,
            "pids": bypass_members(),
            "map": *bypass_map().lock().unwrap()
        })),
        "bypass-clear" => {
            let groups: std::collections::HashMap<String, Vec<u32>> = {
                let mut m = bypass_map().lock().unwrap();
                let g: std::collections::HashMap<String, Vec<u32>> =
                    m.drain().fold(Default::default(), |mut acc, (p, s)| {
                        acc.entry(s).or_default().push(p); acc });
                g
            };
            let mut n = 0;
            for (src, pids) in groups {
                let srcpath = format!("/sys/fs/cgroup{src}");
                for p in pids {
                    if std::path::Path::new(&format!("/proc/{p}")).exists()
                        && std::fs::write(format!("{srcpath}/cgroup.procs"), format!("{p}\n")).is_ok() {
                        n += 1;
                    }
                }
            }
            bypass_map_save();
            Some(json!({"success": true, "action": action, "moved": n}))
        }
        "shutdown" => {
            {
                let mut p = procs.lock().unwrap();
                for e in p.values() {
                    if let Some(pid) = e.pid {
                        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                    }
                    tun_teardown(&e.spec);
                }
            }
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                std::process::exit(0);
            });
            Some(ok(action, "shutting down"))
        }
        _ => Some(err(action, &format!("Unknown action: {action}"))),
    }
}

fn cmd_start(msg: &Value, owner: u64, procs: &Arc<Mutex<HashMap<String, ProcEntry>>>, bus: &GlobalBus) -> Value {
    let id = msg.get("process-id").and_then(|v| v.as_str()).unwrap_or("");
    let exe = msg.get("executable").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return err("start", "Missing or empty process-id");
    }
    if exe.is_empty() {
        return err("start", "Missing or empty executable");
    }
    let args: Vec<String> = msg
        .get("arguments")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let env: Vec<(String, String)> = msg
        .get("environment")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let spec = ProcSpec {
        id: id.to_string(),
        executable: exe.to_string(),
        args,
        env,
        auto_restart: msg.get("auto-restart").and_then(|v| v.as_bool()).unwrap_or(false),
        max_restart_attempts: msg
            .get("max-restart-attempts")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as u32,
        restart_delay_ms: msg.get("restart-delay-ms").and_then(|v| v.as_u64()).unwrap_or(2000),
        // detached: survive the starting client's disconnect (one-shot CLI);
        // unknown keys are ignored by stock happd and by stock clients.
        owner: if msg.get("detached").and_then(|v| v.as_bool()).unwrap_or(false) {
            u64::MAX
        } else {
            owner
        },
        detached: msg.get("detached").and_then(|v| v.as_bool()).unwrap_or(false),
    };

    // stop-and-replace semantics like happd ("Replacing running process")
    {
        let mut p = procs.lock().unwrap();
        if let Some(old) = p.get_mut(id) {
            old.stopping = true;
            if let Some(pid) = old.pid {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }
    match spawn(spec.clone()) {
        Ok((child, entry)) => {
            procs.lock().unwrap().insert(id.to_string(), entry);
            // watchdog owns the Child: waits, updates state, restarts on crash
            let procs2 = procs.clone();
            let bus2 = bus.clone();
            std::thread::spawn(move || watchdog(child, spec, procs2, bus2));
            json!({"success": true, "action": "start",
                   "message": format!("Started process {id}"), "process-id": id})
        }
        Err(e) => err("start", &format!("Failed to start process: {e}")),
    }
}

fn spawn(spec: ProcSpec) -> Result<(Child, ProcEntry), std::io::Error> {
    apply_tun_sysctls(&spec);
    if tun_env(&spec).is_some() {
        // маршрутизация ставится после появления интерфейса (поток ждёт его сам)
        let s = spec.clone();
        std::thread::spawn(move || tun_setup(s));
    }
    let mut cmd = Command::new(&spec.executable);
    cmd.args(&spec.args);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    // логи управляемого процесса — в /var/log/omaxy-<id>.log (diagnostics)
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("/var/log/omaxy-{}.log", spec.id))
        .ok();
    cmd.stdin(Stdio::piped());
    // управляемые процессы умирают вместе с демоном (xray не должен осиротеть)
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    match (&log, &log) {
        (Some(f), Some(g)) => {
            cmd.stdout(Stdio::from(g.try_clone()?)).stderr(Stdio::from(f.try_clone()?));
        }
        _ => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let stdin_tx = child.stdin.take().map(|mut si| {
        let (s, r) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(b) = r.recv() {
                let _ = si.write_all(&b);
                let _ = si.flush();
            }
        });
        s
    });
    let entry = ProcEntry {
        spec,
        state: "started".into(),
        pid: Some(pid),
        started_at: now_ts(),
        attempts: 0,
        stopping: false,
        stdin_tx,
    };
    Ok((child, entry))
}

/// Owns the child; waits for exit, updates state, restarts if configured.
fn watchdog(mut child: Child, spec: ProcSpec, procs: Arc<Mutex<HashMap<String, ProcEntry>>>, bus: GlobalBus) {
    loop {
        let status = child.wait();
        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let do_restart = {
            let mut p = procs.lock().unwrap();
            match p.get_mut(&spec.id) {
                Some(e) => {
                    e.pid = None;
                    e.stdin_tx = None;
                    if e.stopping || code == 0 {
                        e.state = "stopped".into();
                        false
                    } else {
                        let restart = spec.auto_restart && e.attempts < spec.max_restart_attempts;
                        e.state = "crashed".into();
                        if restart {
                            e.attempts += 1;
                        }
                        restart
                    }
                }
                None => false,
            }
        };
        let _ = bus.tx.send(json!({
            "event": "process-event", "process-id": spec.id,
            "state": if do_restart { "restarting" } else if code == 0 { "stopped" } else { "crashed" },
            "exit-code": code
        }));
        if !do_restart {
            // не снимать маршруты, если этот pid уже заменён новым экземпляром
            let owned_now = procs
                .lock()
                .unwrap()
                .get(&spec.id)
                .and_then(|e| e.pid)
                .is_some();
            if !owned_now {
                tun_teardown(&spec);
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(spec.restart_delay_ms));
        match spawn(spec.clone()) {
            Ok((mut new_child, mut entry)) => {
                let mut p = procs.lock().unwrap();
                let replaced_running = match p.get_mut(&spec.id) {
                    Some(prev) if prev.stopping => false, // user stopped it meanwhile
                    Some(prev) => {
                        entry.attempts = prev.attempts;
                        prev.pid = entry.pid;
                        prev.started_at = entry.started_at;
                        prev.state = "started".into();
                        prev.stdin_tx = entry.stdin_tx.take();
                        prev.spec = entry.spec.clone();
                        true
                    }
                    None => {
                        p.insert(spec.id.clone(), entry);
                        true
                    }
                };
                if !replaced_running {
                    let _ = new_child.kill();
                    return;
                }
                child = new_child;
            }
            Err(e) => {
                let _ = bus.tx.send(json!({
                    "event":"process-event","process-id":spec.id,
                    "state":"crashed","message":format!("restart failed: {e}")
                }));
                return;
            }
        }
    }
}

fn cmd_stop(msg: &Value, procs: &Arc<Mutex<HashMap<String, ProcEntry>>>, force: bool) -> Value {
    let id = msg.get("process-id").and_then(|v| v.as_str()).unwrap_or("");
    let mut procs = procs.lock().unwrap();
    if let Some(p) = procs.get_mut(id) {
        p.stopping = true;
        p.state = "stopping".into();
        if let Some(pid) = p.pid {
            unsafe {
                libc::kill(pid as i32, if force { libc::SIGKILL } else { libc::SIGTERM });
            }
        }
        ok("stop", &format!("Process stop requested {id}"))
    } else {
        err("stop", &format!("Process not found: {id}"))
    }
}

/// Happ requires rp_filter loose for TUN; apply the kernel knobs ourselves (root).
fn apply_tun_sysctls(spec: &ProcSpec) {
    if !spec.env.iter().any(|(k, v)| k == "OMAXY_TUN" && v == "1") {
        return;
    }
    for path in [
        "/proc/sys/net/ipv4/conf/all/rp_filter",
        "/proc/sys/net/ipv4/conf/default/rp_filter",
    ] {
        let _ = std::fs::write(path, "2");
    }
    let _ = std::fs::write("/proc/sys/net/ipv4/ip_forward", "1");
}

// ---------- TUN policy routing (xray >=25 не ставит маршруты сам) ----------

fn tun_get(spec: &ProcSpec, key: &str) -> Option<String> {
    spec.env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// Запускать только если spec — туннельный процесс.
fn tun_env(spec: &ProcSpec) -> Option<()> {
    matches!(tun_get(spec, "OMAXY_TUN").as_deref(), Some("1")).then_some(())
}

fn ip_cmd(args: &[&str]) {
    let _ = Command::new("ip")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Ждёт интерфейс, потом поднимает policy-таблицу 88 и DNS через resolved.
fn tun_setup(spec: ProcSpec) {
    let iface = tun_get(&spec, "OMAXY_TUN_IF").unwrap_or_else(|| "omaxy0".into());
    let server = tun_get(&spec, "OMAXY_TUN_SERVER").unwrap_or_default();
    let dns = tun_get(&spec, "OMAXY_TUN_DNS").unwrap_or_default();

    let dev = format!("/sys/class/net/{iface}");
    for _ in 0..100 {
        if std::path::Path::new(&dev).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !std::path::Path::new(&dev).exists() {
        eprintln!("tun_setup: interface {iface} did not appear");
        return;
    }
    // DNS-настройки прошлой сессии на omaxy0 ломают резолв имени сервера — откатить
    let _ = Command::new("resolvectl").args(["revert", &iface]).status();
    // обход приватных/линк-локальных сетей — в main
    let lan = [
        "0.0.0.0/8",
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "224.0.0.0/4",
    ];
    for (i, c) in lan.iter().enumerate() {
        let prio = format!("{}", 30400 + i);
        ip_cmd(&["rule", "del", "priority", &prio]);
        ip_cmd(&["rule", "add", "priority", &prio, "to", c, "lookup", "main"]);
    }
    // обход адреса сервера (иначе петля): сначала IP из CLI, потом свой резолв (×3)
    {
        let mut ips: Vec<String> = tun_get(&spec, "OMAXY_TUN_SERVER_IPS")
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
            .unwrap_or_default();
        if ips.is_empty() && !server.is_empty() {
            let host = server.split(':').next().unwrap_or("");
            for _ in 0..3 {
                if let Ok(addrs) = (host, 0u16).to_socket_addrs() {
                    ips = addrs.filter(|a| a.is_ipv4()).map(|a| a.to_string()).collect();
                    if !ips.is_empty() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        for (i, a) in ips.iter().take(8).enumerate() {
            let prio = format!("{}", 30500 + i);
            ip_cmd(&["rule", "del", "priority", &prio]);
            ip_cmd(&["rule", "add", "priority", &prio, "to", &format!("{a}/32"),
                     "lookup", "main"]);
        }
    }
    // обход DNS-серверов: без этого xray не может разрешить имя своего сервера
    // (DNS уходит в туннель, который поднимается этим xray — бутстрап-петля)
    if !dns.is_empty() {
        for (i, d) in dns.split(',').map(|s| s.trim()).enumerate().take(8) {
            if d.parse::<std::net::Ipv4Addr>().is_ok() {
                let prio = format!("{}", 30450 + i);
                ip_cmd(&["rule", "del", "priority", &prio]);
                ip_cmd(&["rule", "add", "priority", &prio, "to", &format!("{d}/32"),
                         "lookup", "main"]);
            }
        }
    }
    ip_cmd(&["route", "replace", "default", "dev", &iface, "table", "88"]);
    ip_cmd(&["rule", "del", "priority", "31000"]);
    ip_cmd(&["rule", "add", "priority", "31000", "from", "all", "lookup", "88"]);
    if !dns.is_empty() {
        let mut a: Vec<String> = vec!["dns".into(), iface.clone()];
        a.extend(dns.split(',').map(|s| s.trim().to_string()));
        let _ = Command::new("resolvectl").args(&a).status();
        let _ = Command::new("resolvectl")
            .args(["domain", &iface, "~."])
            .status();
        let _ = Command::new("resolvectl")
            .args(["default-route", &iface, "yes"])
            .status();
    }
    bypass_apply(); // обходы приложений живут поверх туннельного захвата
    eprintln!("tun: routing up on {iface}");
}

fn tun_teardown(spec: &ProcSpec) {
    if tun_env(spec).is_none() {
        return;
    }
    bypass_off();
    let iface = tun_get(spec, "OMAXY_TUN_IF").unwrap_or_else(|| "omaxy0".into());
    let server = tun_get(spec, "OMAXY_TUN_SERVER").unwrap_or_default();
    for i in 0..8 {
        ip_cmd(&["rule", "del", "priority", &format!("{}", 30400 + i)]);
    }
    if !server.is_empty() {
        let host = server.split(':').next().unwrap_or("");
        if let Ok(addrs) = (host, 0u16).to_socket_addrs() {
            for (i, a) in addrs.filter(|a| a.is_ipv4()).enumerate().take(8) {
                let _ = a;
                ip_cmd(&["rule", "del", "priority", &format!("{}", 30500 + i)]);
            }
        }
    }
    ip_cmd(&["rule", "del", "priority", "31000"]);
    ip_cmd(&["route", "flush", "table", "88"]);
    let _ = Command::new("resolvectl").args(["revert", &iface]).status();
    eprintln!("tun: routing down on {iface}");
}


// ---------- обход туннеля для выбранных приложений ----------
// Приложение целиком (вся его systemd-scope) переносится в cgroup omaxy-bypass;
// nft помечает its пакеты fwmark'ом, правило 30380 отправляет их в main (мимо 88).
pub const BYPASS_CG: &str = "/sys/fs/cgroup/omaxy-bypass";
const BYPASS_MARK: &str = "0x4b1";
const BYPASS_PRIO: &str = "30380";
const BYPASS_MAP: &str = "/var/lib/omaxy/bypass-map.json";

fn bypass_map() -> &'static std::sync::Mutex<std::collections::HashMap<u32, String>> {
    use std::sync::{Mutex, OnceLock};
    static M: OnceLock<Mutex<std::collections::HashMap<u32, String>>> = OnceLock::new();
    M.get_or_init(|| {
        let map = std::fs::read_to_string(BYPASS_MAP)
            .ok()
            .and_then(|s| serde_json::from_str::<std::collections::HashMap<u32, String>>(&s).ok())
            .unwrap_or_default();
        Mutex::new(map)
    })
}

fn bypass_map_save() {
    if let Ok(s) = serde_json::to_string(&*bypass_map().lock().unwrap()) {
        let _ = std::fs::write(BYPASS_MAP, s);
    }
}

/// id cgroup = inode каталога (для nft meta cgroup)
fn bypass_ino() -> u64 {
    use std::os::unix::fs::MetadataExt;
    let _ = std::fs::create_dir(BYPASS_CG);
    std::fs::metadata(BYPASS_CG).map(|m| m.ino()).unwrap_or(0)
}

fn bypass_apply() {
    let ino = bypass_ino();
    if ino == 0 {
        return;
    }
    let ruleset = format!(
        "table inet omaxy_bypass {{\n\tchain out {{\n\t\ttype filter hook output priority mangle; policy accept;\n\t\tmeta cgroup {ino} counter meta mark set {BYPASS_MARK}\n\t}}\n}}\n"
    );
    let _ = std::fs::write("/run/omaxy-bypass.nft", &ruleset);
    let _ = Command::new("sh")
        .args(["-c", "nft delete table inet omaxy_bypass 2>/dev/null; nft -f /run/omaxy-bypass.nft"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    ip_cmd(&["rule", "del", "priority", BYPASS_PRIO]);
    ip_cmd(&["rule", "add", "priority", BYPASS_PRIO, "fwmark", BYPASS_MARK, "lookup", "main"]);
}

fn bypass_off() {
    ip_cmd(&["rule", "del", "priority", BYPASS_PRIO]);
    let _ = Command::new("nft")
        .args(["delete", "table", "inet", "omaxy_bypass"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// путь cgroup v2 процесса (0::/...)
fn pid_cgroup(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let p = s.lines().find_map(|l| l.strip_prefix("0::"))?.trim().to_string();
    if p == "/" || p.is_empty() { None } else { Some(p) }
}

/// перенести все процессы из cgroup `src` в dst (или в src при обратном); возвращает число
fn bypass_move_all(src: &str, dst_cg: &str) -> usize {
    let procs = std::path::Path::new(src).join("cgroup.procs");
    let Ok(list) = std::fs::read_to_string(&procs) else { return 0 };
    let mut n = 0;
    for p in list.lines().filter_map(|l| l.trim().parse::<u32>().ok()) {
        if std::fs::write(format!("{dst_cg}/cgroup.procs"), format!("{p}\n")).is_ok() {
            n += 1;
        }
    }
    n
}

fn bypass_members() -> Vec<u32> {
    std::fs::read_to_string(format!("{BYPASS_CG}/cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
}

use std::net::ToSocketAddrs;

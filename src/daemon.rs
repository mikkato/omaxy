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
        "shutdown" => Some(ok(action, "shutting down")),
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
    let mut cmd = Command::new(&spec.executable);
    cmd.args(&spec.args);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
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

//! omaxy-gui — компактное Slint-окно поверх CLI `omaxy`.
//! Все вызовы CLI идут в отдельный поток, результат возвращается
//! в UI через slint::invoke_from_event_loop.
#![windows_subsystem = "windows"]

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

slint::include_modules!();

/// Где искать бинарник omaxy: $OMAXY_BIN → рядом с самим omaxy-gui → PATH.
fn omaxy_bin() -> PathBuf {
    if let Ok(p) = std::env::var("OMAXY_BIN") {
        if !p.is_empty() {
            return p.into();
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("omaxy");
            if cand.exists() {
                return cand;
            }
        }
    }
    PathBuf::from("omaxy")
}

fn run_cli(args: &[&str]) -> Result<String, String> {
    let out = Command::new(omaxy_bin())
        .args(args)
        .output()
        .map_err(|e| format!("не удалось запустить omaxy: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("omaxy {} завершился с кодом {:?}", args.join(" "), out.status.code())
        } else {
            err
        })
    }
}

/// Выполняет job в фоновом потоке, вызывает done(result) в цикле событий Slint.
fn spawn_job<F, G>(weak: &Weak<OmaxyWindow>, job: F, done: G)
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
    G: FnOnce(Result<String, String>) + Send + 'static,
{
    let w = weak.clone();
    std::thread::spawn(move || {
        let res = job();
        let _ = slint::invoke_from_event_loop(move || {
            if w.upgrade().is_some() {
                done(res);
            }
        });
    });
}

fn set_msg(ui: &OmaxyWindow, msg: String, error: bool) {
    ui.set_message(msg.into());
    ui.set_message_error(error);
}

/// Одиночная CLI-операция с busy-блокировкой, сообщением и обновлением статуса.
fn action(
    ui: &OmaxyWindow,
    verb: &'static str,
    args: Vec<String>,
    job: Option<Box<dyn FnOnce() -> Result<String, String> + Send>>,
) {
    if ui.get_busy() {
        return;
    }
    ui.set_busy(true);
    set_msg(ui, format!("{verb}…"), false);
    let weak = ui.as_weak();
    let run = move || match job {
        Some(j) => j(),
        None => run_cli(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
    };
    spawn_job(&weak.clone(), run, move |res| {
        let Some(ui) = weak.upgrade() else { return };
        match &res {
            Ok(out) => set_msg(
                &ui,
                format!("{verb}: {}", if out.is_empty() { "готово" } else { out.as_str() }),
                false,
            ),
            Err(e) => set_msg(&ui, format!("{verb}: {e}"), true),
        }
        ui.set_busy(false);
        refresh(&ui);
    });
}

fn apply_status(ui: &OmaxyWindow, out: &str) {
    let v: serde_json::Value = match serde_json::from_str(out) {
        Ok(v) => v,
        Err(_) => return,
    };
    let daemon = v["daemon"].as_bool().unwrap_or(false);
    let connected = v["connected"].as_bool().unwrap_or(false);
    ui.set_daemon_running(daemon);
    ui.set_connected(connected);
    ui.set_status_text(if !daemon {
        SharedString::from("демон не запущен")
    } else if connected {
        let name = v["active"]["name"].as_str().unwrap_or("?");
        let proto = v["active"]["proto"].as_str().unwrap_or("?");
        format!("подключено: {name} ({proto})").into()
    } else {
        SharedString::from("выключено")
    });

    // сохраняем выбор по id при перестроении модели
    let prev_sel_id = {
        let i = ui.get_selected_index();
        let cur = ui.get_servers();
        if i >= 0 && (i as usize) < cur.row_count() {
            cur.row_data(i as usize).map(|r| r.id)
        } else {
            None
        }
    };

    let mut sel = -1i32;
    let rows: Vec<ServerRow> = v["servers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(idx, s)| {
                    let id: SharedString = s["id"].as_str().unwrap_or_default().into();
                    if prev_sel_id.as_ref().is_some_and(|p| *p == id) {
                        sel = idx as i32;
                    }
                    ServerRow {
                        id,
                        name: s["name"].as_str().unwrap_or("?").into(),
                        proto: s["proto"].as_str().unwrap_or("?").into(),
                        address: s["address"].as_str().unwrap_or("").into(),
                        port: s["port"].as_i64().unwrap_or(0) as i32,
                        active: s["active"].as_bool().unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ui.set_servers(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_selected_index(sel);

    let subs: Vec<SubRow> = v["subscriptions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|s| SubRow {
                    id: s["id"].as_str().unwrap_or_default().into(),
                    name: s["name"].as_str().unwrap_or("?").into(),
                    count: s["count"].as_i64().unwrap_or(0) as i32,
                })
                .collect()
        })
        .unwrap_or_default();
    ui.set_subscriptions(ModelRc::from(Rc::new(VecModel::from(subs))));
}

fn refresh(ui: &OmaxyWindow) {
    let weak = ui.as_weak();
    spawn_job(&weak.clone(), || run_cli(&["status", "--json"]), move |res| {
        let Some(ui) = weak.upgrade() else { return };
        match res {
            Ok(out) => apply_status(&ui, &out),
            Err(e) => {
                ui.set_daemon_running(false);
                ui.set_connected(false);
                ui.set_status_text(format!("статус недоступен: {e}").into());
            }
        }
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = OmaxyWindow::new()?;
    // принудительно тёмная схема для стандартных виджетов (fluent)
    let palette: Palette = ui.global::<Palette>();
    palette.set_color_scheme(slint::language::ColorScheme::Dark);

    ui.on_refresh_request({
        let w = ui.as_weak();
        move || {
            if let Some(ui) = w.upgrade() {
                refresh(&ui);
            }
        }
    });

    ui.on_connect_request({
        let w = ui.as_weak();
        move |id| {
            let Some(ui) = w.upgrade() else { return };
            let args: Vec<String> = if id.is_empty() {
                vec!["connect".into()]
            } else {
                vec!["connect".into(), id.to_string()]
            };
            action(&ui, "подключение", args, None);
        }
    });

    ui.on_disconnect_request({
        let w = ui.as_weak();
        move || {
            if let Some(ui) = w.upgrade() {
                action(&ui, "отключение", vec!["disconnect".into()], None);
            }
        }
    });

    ui.on_add_link_request({
        let w = ui.as_weak();
        move |link| {
            let Some(ui) = w.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            let args = vec!["server".to_string(), "add".to_string(), link.to_string()];
            ui.set_link_input(SharedString::default());
            action(&ui, "добавление сервера", args, None);
        }
    });

    ui.on_remove_server_request({
        let w = ui.as_weak();
        move |id| {
            let Some(ui) = w.upgrade() else { return };
            action(
                &ui,
                "удаление сервера",
                vec!["server".into(), "rm".into(), id.to_string()],
                None,
            );
        }
    });

    ui.on_qr_import_request({
        let w = ui.as_weak();
        move |path| {
            let Some(ui) = w.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            let path = path.to_string();
            // сначала декодируем QR, потом добавляем полученную ссылку
            let job = Box::new(move || -> Result<String, String> {
                let link = run_cli(&["qr", &path])?;
                let link = link.lines().next().unwrap_or("").trim().to_string();
                if link.is_empty() {
                    return Err("QR не содержит ссылку".into());
                }
                run_cli(&["server", "add", &link])
            });
            action(&ui, "QR-импорт", vec![], Some(job));
        }
    });

    ui.on_subscribe_update_request({
        let w = ui.as_weak();
        move || {
            if let Some(ui) = w.upgrade() {
                action(&ui, "обновление подписок", vec!["subscribe".into(), "update".into()], None);
            }
        }
    });

    // первоначальный статус + автоопрос раз в 3 секунды
    refresh(&ui);
    let timer = Timer::default();
    let w = ui.as_weak();
    timer.start(TimerMode::Repeated, Duration::from_secs(3), move || {
        if let Some(ui) = w.upgrade() {
            refresh(&ui);
        }
    });
    let _timer_keepalive = timer;

    ui.run()
}
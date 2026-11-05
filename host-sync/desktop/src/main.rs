#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;

use anyhow::{ensure, Context, Result};
use frame_host_sync::{serial::Connection, watch_calendar, Config, WatchControl, WatchEvent};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

#[derive(Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    Stopped,
    Running,
    Stopping,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ImportOutcome {
    Imported,
    Cancelled,
}

#[derive(Clone, Default, Serialize)]
struct Status {
    config: Option<PathBuf>,
    configured: bool,
    phase: Phase,
    syncing: bool,
    last_synced: Option<String>,
    events: Option<usize>,
    calendar_error: Option<String>,
    privacy_error: Option<String>,
    privacy_supported: bool,
}

#[derive(Default)]
struct Runtime {
    status: Status,
    control: Option<mpsc::Sender<WatchControl>>,
    quitting: bool,
}

struct Desktop(Mutex<Runtime>);

struct TrayControls {
    status: MenuItem<tauri::Wry>,
    start: MenuItem<tauri::Wry>,
    stop: MenuItem<tauri::Wry>,
    sync: MenuItem<tauri::Wry>,
}

#[tauri::command]
fn status(app: AppHandle) -> Status {
    app.state::<Desktop>().0.lock().unwrap().status.clone()
}

fn publish(app: &AppHandle) {
    let status = status(app.clone());
    let text = if status.phase == Phase::Stopping {
        "Stopping sync…"
    } else if status.phase == Phase::Stopped {
        "Sync stopped"
    } else if status.calendar_error.is_some() || status.privacy_error.is_some() {
        "Sync needs attention"
    } else if status.syncing {
        "Syncing calendar…"
    } else {
        "Sync running"
    };
    if let Some(tray) = app.tray_by_id("sync") {
        let _ = tray.set_tooltip(Some(format!("Frame Host Sync · {text}")));
    }
    let controls = app.state::<TrayControls>();
    let _ = controls.status.set_text(text);
    let _ = controls
        .start
        .set_enabled(status.phase == Phase::Stopped && status.configured);
    let _ = controls.stop.set_enabled(status.phase == Phase::Running);
    let _ = controls
        .sync
        .set_enabled(status.phase == Phase::Running && !status.syncing);
}

fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn start(app: &AppHandle) -> Result<()> {
    let desktop = app.state::<Desktop>();
    let mut runtime = desktop.0.lock().unwrap();
    ensure!(
        runtime.status.phase == Phase::Stopped,
        "Sync is already running or stopping."
    );
    let path = runtime
        .status
        .config
        .as_ref()
        .context("Default configuration is unavailable.")?;
    let config = Config::load(path)?;
    let connection = config
        .connection
        .as_ref()
        .context("Sync requires a connection in config.")?;
    let privacy_supported = matches!(connection, Connection::Console { .. });
    let base = path
        .parent()
        .context("Configuration must have a parent directory.")?
        .to_owned();
    let (control, commands) = mpsc::channel();
    runtime.control = Some(control);
    runtime.status.phase = Phase::Running;
    runtime.status.calendar_error = None;
    runtime.status.privacy_error = None;
    runtime.status.privacy_supported = privacy_supported;
    drop(runtime);
    publish(app);
    let app = app.clone();
    std::thread::spawn(move || {
        let result = watch_calendar(config, base, commands, |event| {
            let desktop = app.state::<Desktop>();
            let mut runtime = desktop.0.lock().unwrap();
            match event {
                WatchEvent::Syncing => runtime.status.syncing = true,
                WatchEvent::Synced { at, events } => {
                    runtime.status.syncing = false;
                    runtime.status.last_synced = Some(at);
                    runtime.status.events = Some(events);
                    runtime.status.calendar_error = None;
                }
                WatchEvent::CalendarFailed(error) => {
                    runtime.status.syncing = false;
                    runtime.status.calendar_error = Some(error);
                }
                WatchEvent::Privacy { error } => runtime.status.privacy_error = error,
            }
            drop(runtime);
            publish(&app);
        });
        let desktop = app.state::<Desktop>();
        let mut runtime = desktop.0.lock().unwrap();
        runtime.status.phase = Phase::Stopped;
        runtime.status.syncing = false;
        runtime.control = None;
        if let Err(error) = result {
            runtime.status.calendar_error = Some(format!("{error:#}"));
        }
        let quitting = runtime.quitting;
        drop(runtime);
        publish(&app);
        if quitting {
            app.exit(0);
        }
    });
    Ok(())
}

fn stop(app: &AppHandle, quitting: bool) {
    let desktop = app.state::<Desktop>();
    let mut runtime = desktop.0.lock().unwrap();
    runtime.quitting |= quitting;
    if let Some(control) = &runtime.control {
        let _ = control.send(WatchControl::Stop);
        runtime.status.phase = Phase::Stopping;
    }
    let exit = quitting && runtime.status.phase == Phase::Stopped;
    drop(runtime);
    publish(app);
    if exit {
        app.exit(0);
    }
}

#[tauri::command]
fn start_sync(app: AppHandle) -> Result<(), String> {
    start(&app).map_err(|error| format!("{error:#}"))
}

#[tauri::command]
fn stop_sync(app: AppHandle) {
    stop(&app, false);
}

#[tauri::command]
fn sync_now(app: AppHandle) -> Result<(), String> {
    let desktop = app.state::<Desktop>();
    let runtime = desktop.0.lock().unwrap();
    if runtime.status.phase != Phase::Running {
        return Err("Start sync first.".into());
    }
    runtime
        .control
        .as_ref()
        .unwrap()
        .send(WatchControl::SyncNow)
        .map_err(|_| "Sync worker stopped.".into())
}

#[tauri::command]
fn load_config(app: AppHandle) -> Result<String, String> {
    let snapshot = status(app);
    let path = snapshot
        .config
        .ok_or("Default configuration is unavailable.")?;
    std::fs::read_to_string(path).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_config(app: AppHandle, contents: String) -> Result<(), String> {
    let result = (|| -> Result<()> {
        let desktop = app.state::<Desktop>();
        let mut runtime = desktop.0.lock().unwrap();
        ensure!(
            runtime.status.phase == Phase::Stopped,
            "Stop sync before changing configuration."
        );
        let path = runtime
            .status
            .config
            .as_ref()
            .context("Default configuration is unavailable.")?;
        config::save(path, contents.as_bytes())?;
        runtime.status = Status {
            config: Some(path.clone()),
            configured: true,
            ..Status::default()
        };
        drop(runtime);
        publish(&app);
        Ok(())
    })();
    result.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn choose_config(app: AppHandle) -> Result<ImportOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<ImportOutcome> {
        ensure!(
            status(app.clone()).phase == Phase::Stopped,
            "Stop sync before changing configuration."
        );
        let Some(file) = app
            .dialog()
            .file()
            .add_filter("Host sync configuration", &["json"])
            .blocking_pick_file()
        else {
            return Ok(ImportOutcome::Cancelled);
        };
        let path = file.into_path()?.canonicalize()?;
        let desktop = app.state::<Desktop>();
        let mut runtime = desktop.0.lock().unwrap();
        ensure!(
            runtime.status.phase == Phase::Stopped,
            "Stop sync before changing configuration."
        );
        let target = runtime
            .status
            .config
            .as_ref()
            .context("Default configuration is unavailable.")?;
        config::import(target, &path)?;
        runtime.status = Status {
            config: Some(target.clone()),
            configured: true,
            ..Status::default()
        };
        drop(runtime);
        publish(&app);
        Ok(ImportOutcome::Imported)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("{error:#}"))
}

fn report_action(app: &AppHandle, result: Result<()>) {
    if let Err(error) = result {
        app.state::<Desktop>()
            .0
            .lock()
            .unwrap()
            .status
            .calendar_error = Some(format!("{error:#}"));
        publish(app);
        show(app);
    }
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_dialog::init())
        .manage(Desktop(Mutex::new(Runtime::default())))
        .invoke_handler(tauri::generate_handler![
            status,
            start_sync,
            stop_sync,
            sync_now,
            choose_config,
            load_config,
            save_config
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let open = MenuItem::with_id(app, "open", "Open Frame Host Sync", true, None::<&str>)?;
            let status = MenuItem::with_id(app, "status", "Sync stopped", false, None::<&str>)?;
            let start_item = MenuItem::with_id(app, "start", "Start sync", false, None::<&str>)?;
            let sync = MenuItem::with_id(app, "sync", "Sync now", false, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "Stop sync", false, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Frame Host Sync", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status,
                    &separator,
                    &open,
                    &start_item,
                    &sync,
                    &stop_item,
                    &quit,
                ],
            )?;
            TrayIconBuilder::with_id("sync")
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("Frame Host Sync")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show(app),
                    "start" => report_action(app, start(app)),
                    "sync" => report_action(app, sync_now(app.clone()).map_err(anyhow::Error::msg)),
                    "stop" => stop(app, false),
                    "quit" => stop(app, true),
                    _ => {}
                })
                .build(app)?;
            app.manage(TrayControls {
                status,
                start: start_item,
                sync,
                stop: stop_item,
            });
            let (path, created) = config::initialize(&app.path().home_dir()?)?;
            let loaded = Config::load(&path).and_then(|config| {
                ensure!(
                    config.connection.is_some(),
                    "Set connection.port before starting sync."
                );
                Ok(())
            });
            {
                let desktop = app.state::<Desktop>();
                let mut runtime = desktop.0.lock().unwrap();
                runtime.status.config = Some(path);
                runtime.status.configured = loaded.is_ok();
            }
            if created {
                show(app.handle());
            } else {
                report_action(app.handle(), loaded.and_then(|_| start(app.handle())));
            }
            publish(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("could not start Frame Host Sync");
    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. }
            if status(app.clone()).phase != Phase::Stopped =>
        {
            api.prevent_exit();
            stop(app, true);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show(app),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::ImportOutcome;

    #[test]
    fn import_outcomes_distinguish_replacement_from_cancellation_on_the_wire() {
        assert_eq!(
            serde_json::to_value(ImportOutcome::Imported).unwrap(),
            "imported"
        );
        assert_eq!(
            serde_json::to_value(ImportOutcome::Cancelled).unwrap(),
            "cancelled"
        );
    }
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod download;
mod engine;
mod paths;

use download::{Job, Summary};
use engine::Engines;
use paths::Settings;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{oneshot, Mutex, OnceCell};

#[derive(Default)]
struct AppState {
    engines: OnceCell<Result<Engines, String>>,
    running: AtomicBool,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
}

async fn engines(state: &AppState) -> Result<Engines, String> {
    state
        .engines
        .get_or_init(|| async {
            tokio::task::spawn_blocking(|| engine::prepare(paths::data_dir()))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        })
        .await
        .clone()
}

#[derive(Serialize)]
struct EngineStatus {
    ready: bool,
    version: &'static str,
    error: Option<String>,
}

#[tauri::command]
async fn engine_status(state: State<'_, Arc<AppState>>) -> Result<EngineStatus, ()> {
    let result = engines(&state).await;
    Ok(EngineStatus {
        ready: result.is_ok(),
        version: engine::SPOTDL_VERSION,
        error: result.err(),
    })
}

#[tauri::command]
fn get_settings() -> Settings {
    paths::load_settings()
}

#[tauri::command]
fn save_settings(settings: Settings) -> Result<(), String> {
    paths::save_settings(&settings)
}

#[tauri::command]
async fn browse_folder(app: AppHandle, current: String) -> Option<String> {
    let start = paths::expand_tilde(&current);
    let mut dialog = app.dialog().file().set_title("Choose where to save music");
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    let picked = tokio::task::spawn_blocking(move || dialog.blocking_pick_folder()).await.ok()??;
    picked.into_path().ok().map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    let dir = paths::expand_tilde(&path);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_download(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    query: String,
    settings: Settings,
) -> Result<(), String> {
    let job = Job {
        query,
        output_dir: paths::expand_tilde(&settings.output_dir),
        format: settings.format.clone(),
        bitrate: settings.bitrate.clone(),
        naming: settings.naming.clone(),
    };
    download::validate(&job)?;

    // Stays set until spotDL has fully exited, including after a cancel.
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("A download is already running.".into());
    }
    let prepared = async {
        let engines = engines(&state).await?;
        std::fs::create_dir_all(&job.output_dir)
            .map_err(|e| format!("Could not create {}: {e}", job.output_dir.display()))?;
        Ok::<_, String>(engines)
    }
    .await;
    let engines = match prepared {
        Ok(e) => e,
        Err(e) => {
            state.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    let _ = paths::save_settings(&settings);

    let (tx, rx) = oneshot::channel();
    *state.cancel.lock().await = Some(tx);
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn(async move {
        let summary: Summary = download::run(app.clone(), job, engines, rx).await;
        state.cancel.lock().await.take();
        state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("done", summary);
    });
    Ok(())
}

#[tauri::command]
async fn cancel_download(state: State<'_, Arc<AppState>>) -> Result<(), ()> {
    if let Some(tx) = state.cancel.lock().await.take() {
        let _ = tx.send(());
    }
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(AppState::default()))
        .setup(|app| {
            // Unpack engines while the window loads instead of on the first download.
            let state = Arc::clone(app.state::<Arc<AppState>>().inner());
            tauri::async_runtime::spawn(async move {
                let _ = engines(&state).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_status,
            get_settings,
            save_settings,
            browse_folder,
            open_folder,
            start_download,
            cancel_download
        ])
        .run(tauri::generate_context!())
        .expect("failed to start spotDL Studio");
}

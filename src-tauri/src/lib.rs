//! # StageBadger — Tauri Command Layer
//!
//! This module registers all Tauri IPC commands and manages the global application state.
//! Every function annotated with `#[tauri::command]` is callable from the frontend via `invoke()`.

pub mod asr;
pub mod chat;
pub mod ffmpeg;

use std::sync::Arc;
use tokio::sync::Mutex;

use ffmpeg::{AvDevices, StreamState};

/// Enumerate all available audio/video devices via FFmpeg's AVFoundation listing.
#[tauri::command]
async fn get_av_devices() -> Result<AvDevices, String> {
    ffmpeg::get_devices().await
}

/// Start a broadcast session.
///
/// Spawns the FFmpeg child process with the configured filter graph,
/// then kicks off the ASR and chat background workers.
#[tauri::command]
async fn start_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    server_url: String,
    youtube_key: String,
    camera_id: String,
    mic_id: String,
    enable_recording: bool,
    overlay_path: String,
) -> Result<(), String> {
    ffmpeg::start(app, state, server_url, youtube_key, camera_id, mic_id, enable_recording, overlay_path).await
}

/// Stop the active broadcast session by killing the FFmpeg child process.
#[tauri::command]
async fn stop_stream(state: tauri::State<'_, Arc<Mutex<StreamState>>>) -> Result<(), String> {
    ffmpeg::stop(state).await
}

/// Check whether the external MOE volume is mounted.
///
/// Returns `true` if `/Volumes/MOE` exists on disk (used for model storage
/// and local recording destinations).
#[tauri::command]
async fn check_moe_mount() -> Result<bool, String> {
    Ok(std::path::Path::new("/Volumes/MOE").exists())
}

/// Initialize and run the Tauri application.
///
/// Sets up the shared `StreamState`, registers all IPC commands,
/// and starts the event loop.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = Arc::new(Mutex::new(StreamState::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(desktop)]
            {
                let menu = tauri::menu::Menu::default(app.handle())?;
                app.set_menu(menu)?;
            }
            Ok(())
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_av_devices,
            start_stream,
            stop_stream,
            check_moe_mount
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

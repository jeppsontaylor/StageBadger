//! # StageBadger — Tauri Command Layer
//!
//! This module registers all Tauri IPC commands and manages the global application state.
//! Every function annotated with `#[tauri::command]` is callable from the frontend via `invoke()`.

pub mod asr;
pub mod chat;
pub mod destinations;
pub mod ffmpeg;
pub mod transcript;
pub mod types;

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

use destinations::SecretStore;
use destinations::{
    delete_destination, destinations_path, load_destinations, mark_destination_used, normalize_server_url,
    save_destination, test_rtmp_destination as test_manual_destination, MacOsSecurityKeychain,
    YOUTUBE_LIVE_CONTROL_ROOM_URL,
};
use ffmpeg::{AvDevices, StreamState};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use types::{
    DestinationConfig, DestinationKind, EncoderProfile, LiveSessionRequest, ManualDestination,
    ManualDestinationSaveRequest, ManualDestinationTestRequest, OverlayItem, OverlayStatus, RecordingProfile,
    SessionStatus, YoutubeStatus,
};

/// Enumerate all available audio/video devices via FFmpeg's AVFoundation listing.
#[tauri::command]
async fn get_av_devices() -> Result<AvDevices, String> {
    ffmpeg::get_devices().await
}

/// Start a broadcast session.
///
/// Spawns the FFmpeg child process with the configured filter graph,
/// then kicks off the chat background worker and ASR session.
#[tauri::command]
async fn start_stream(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    asr_state: tauri::State<'_, Arc<StdMutex<transcript::AsrRuntime>>>,
    server_url: String,
    youtube_key: String,
    camera_id: String,
    mic_id: String,
    enable_recording: bool,
    overlay_path: String,
    blur_background: bool,
) -> Result<(), String> {
    use tauri::Emitter;

    // Resolve web-relative overlay path to absolute filesystem path for FFmpeg
    let resolved_overlay = if overlay_path.starts_with('/')
        && !overlay_path.starts_with("/Volumes")
        && !overlay_path.starts_with("/Users")
    {
        // It's a Vite-relative path like "/overlays/cyberpunk.png" → resolve to public/ dir
        let project_root = env!("CARGO_MANIFEST_DIR").replace("/src-tauri", "");
        let candidate = format!("{}/public{}", project_root, overlay_path);
        if std::path::Path::new(&candidate).exists() {
            println!("FFmpeg: Resolved overlay '{}' → '{}'", overlay_path, candidate);
            let _ = app.emit("system_log", format!("FFmpeg: Overlay resolved → {}", candidate));
            candidate
        } else {
            println!("FFmpeg: WARNING — overlay not found at '{}'", candidate);
            let _ = app.emit(
                "system_log",
                format!("FFmpeg: WARNING — overlay not found at {}", candidate),
            );
            overlay_path.clone()
        }
    } else {
        overlay_path.clone()
    };

    let _ = app.emit("system_log", format!("FFmpeg: Starting broadcast to {}", server_url));
    let _ = app.emit(
        "system_log",
        format!(
            "FFmpeg: Camera={}, Mic={}, Recording={}, Blur={}",
            camera_id, mic_id, enable_recording, blur_background
        ),
    );

    ffmpeg::start(
        state,
        server_url,
        youtube_key,
        camera_id,
        mic_id.clone(),
        enable_recording,
        resolved_overlay,
        blur_background,
    )
    .await?;
    let _ = transcript::start_session(
        app.clone(),
        asr_state,
        mic_id,
        "Broadcast session".to_string(),
        ffmpeg::now_millis(),
    )
    .await
    .map_err(|err| {
        let _ = app.emit("system_log", format!("ASR session did not start: {}", err));
        err
    });
    Ok(())
}

/// Stop the active broadcast session by killing the FFmpeg child process.
#[tauri::command]
async fn stop_stream(
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    asr_state: tauri::State<'_, Arc<StdMutex<transcript::AsrRuntime>>>,
) -> Result<(), String> {
    let _ = transcript::stop_session(asr_state).await;
    ffmpeg::stop(state).await
}

fn manual_destination_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    destinations_path(app)
}

fn keychain_youtube_token() -> Option<String> {
    std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "com.jeppsontaylor.stagebadger.youtube",
            "-w",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn youtube_token() -> Option<String> {
    keychain_youtube_token().or_else(|| std::env::var("STAGEBADGER_YOUTUBE_ACCESS_TOKEN").ok())
}

fn youtube_scheduled_start_time() -> String {
    std::process::Command::new("date")
        .args(["-u", "-v", "+5M", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "2030-01-01T00:00:00Z".to_string())
}

/// Connect YouTube using an OAuth token held outside the repo.
#[tauri::command]
async fn connect_youtube(app: tauri::AppHandle) -> Result<YoutubeStatus, String> {
    let status = if youtube_token().is_some() {
        YoutubeStatus {
            connected: true,
            message: "YouTube OAuth token available from the OS keychain".to_string(),
            broadcast_id: None,
            stream_id: None,
            live_chat_id: None,
            ingest_url: None,
            stream_key: None,
        }
    } else {
        YoutubeStatus {
            connected: false,
            message: "Connect YouTube OAuth before using managed RTMPS, or switch to manual RTMP/key.".to_string(),
            broadcast_id: None,
            stream_id: None,
            live_chat_id: None,
            ingest_url: None,
            stream_key: None,
        }
    };

    let _ = app.emit("youtube_status", status.clone());
    Ok(status)
}

#[tauri::command]
async fn open_youtube_live_control_room(app: tauri::AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(YOUTUBE_LIVE_CONTROL_ROOM_URL, None::<&str>)
        .map_err(|e| format!("Failed to open YouTube Live Control Room: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn load_manual_destinations(app: tauri::AppHandle) -> Result<Vec<ManualDestination>, String> {
    let path = manual_destination_path(&app)?;
    let secrets = MacOsSecurityKeychain;
    load_destinations(&path, &secrets)
}

#[tauri::command]
async fn save_manual_destination(
    app: tauri::AppHandle,
    request: ManualDestinationSaveRequest,
) -> Result<ManualDestination, String> {
    let path = manual_destination_path(&app)?;
    let secrets = MacOsSecurityKeychain;
    let saved = save_destination(&path, &secrets, request)?;
    let _ = app.emit("manual_destinations_updated", ());
    Ok(saved)
}

#[tauri::command]
async fn delete_manual_destination(
    app: tauri::AppHandle,
    destination_id: String,
) -> Result<Vec<ManualDestination>, String> {
    let path = manual_destination_path(&app)?;
    let secrets = MacOsSecurityKeychain;
    delete_destination(&path, &secrets, &destination_id)?;
    let _ = app.emit("manual_destinations_updated", ());
    load_destinations(&path, &secrets)
}

#[tauri::command]
async fn test_rtmp_destination(
    app: tauri::AppHandle,
    request: ManualDestinationTestRequest,
) -> Result<types::DestinationTestResult, String> {
    let path = manual_destination_path(&app)?;
    let secrets = MacOsSecurityKeychain;
    test_manual_destination(&path, &secrets, request)
}

async fn resolve_manual_destination(
    app: &tauri::AppHandle,
    mut destination: DestinationConfig,
) -> Result<DestinationConfig, String> {
    let Some(destination_id) = destination.manual_destination_id.clone() else {
        return Ok(destination);
    };

    let path = manual_destination_path(app)?;
    let secrets = MacOsSecurityKeychain;
    let saved = destinations::find_destination(&path, &destination_id)?
        .ok_or_else(|| "Saved destination was not found.".to_string())?;
    let stream_key = secrets
        .load(&destination_id)?
        .ok_or_else(|| "Saved destination is missing its keychain stream key.".to_string())?;

    destination.label = saved.label;
    destination.rtmp_url = Some(normalize_server_url(&saved.server_url)?);
    destination.stream_key = Some(stream_key);
    if destination.kind == DestinationKind::ManualRtmp {
        destination.kind = DestinationKind::YoutubeRtmps;
    }
    let _ = mark_destination_used(&path, &destination_id);
    Ok(destination)
}

/// Create and bind a YouTube broadcast/stream when OAuth credentials are available.
#[tauri::command]
async fn create_broadcast(
    app: tauri::AppHandle,
    title: String,
    privacy_status: String,
) -> Result<YoutubeStatus, String> {
    let Some(token) = youtube_token() else {
        let status = YoutubeStatus {
            connected: false,
            message: "YouTube OAuth is not connected. Manual RTMP/key fallback is available.".to_string(),
            broadcast_id: None,
            stream_id: None,
            live_chat_id: None,
            ingest_url: None,
            stream_key: None,
        };
        let _ = app.emit("youtube_status", status.clone());
        return Ok(status);
    };

    let client = reqwest::Client::new();
    let broadcast_response: serde_json::Value = client
        .post("https://www.googleapis.com/youtube/v3/liveBroadcasts?part=snippet,status,contentDetails")
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "snippet": {
                "title": title,
                "scheduledStartTime": youtube_scheduled_start_time()
            },
            "status": {
                "privacyStatus": privacy_status
            },
            "contentDetails": {
                "enableAutoStart": true,
                "enableAutoStop": true
            }
        }))
        .send()
        .await
        .map_err(|e| format!("YouTube broadcast request failed: {}", e))?
        .error_for_status()
        .map_err(|e| format!("YouTube broadcast rejected: {}", e))?
        .json()
        .await
        .map_err(|e| format!("YouTube broadcast response failed: {}", e))?;

    let broadcast_id = broadcast_response
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "YouTube did not return a broadcast id".to_string())?
        .to_string();
    let live_chat_id = broadcast_response
        .pointer("/snippet/liveChatId")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    let stream_response: serde_json::Value = client
        .post("https://www.googleapis.com/youtube/v3/liveStreams?part=snippet,cdn")
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "snippet": {
                "title": "StageBadger RTMPS Stream"
            },
            "cdn": {
                "frameRate": "30fps",
                "ingestionType": "rtmp",
                "resolution": "1080p"
            }
        }))
        .send()
        .await
        .map_err(|e| format!("YouTube stream request failed: {}", e))?
        .error_for_status()
        .map_err(|e| format!("YouTube stream rejected: {}", e))?
        .json()
        .await
        .map_err(|e| format!("YouTube stream response failed: {}", e))?;

    let stream_id = stream_response
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "YouTube did not return a stream id".to_string())?
        .to_string();
    let ingest_url = stream_response
        .pointer("/cdn/ingestionInfo/rtmpsIngestionAddress")
        .or_else(|| stream_response.pointer("/cdn/ingestionInfo/ingestionAddress"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let stream_key = stream_response
        .pointer("/cdn/ingestionInfo/streamName")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    let _bind_response: serde_json::Value = client
        .post(format!(
            "https://www.googleapis.com/youtube/v3/liveBroadcasts/bind?part=id,contentDetails&id={}&streamId={}",
            broadcast_id, stream_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("YouTube bind request failed: {}", e))?
        .error_for_status()
        .map_err(|e| format!("YouTube bind rejected: {}", e))?
        .json()
        .await
        .map_err(|e| format!("YouTube bind response failed: {}", e))?;

    let status = YoutubeStatus {
        connected: true,
        message: "YouTube broadcast and RTMPS stream are bound".to_string(),
        broadcast_id: Some(broadcast_id),
        stream_id: Some(stream_id),
        live_chat_id,
        ingest_url,
        stream_key,
    };
    let _ = app.emit("youtube_status", status.clone());
    Ok(status)
}

#[tauri::command]
async fn start_live_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    asr_state: tauri::State<'_, Arc<StdMutex<transcript::AsrRuntime>>>,
    mut request: LiveSessionRequest,
) -> Result<SessionStatus, String> {
    request.destination = resolve_manual_destination(&app, request.destination).await?;
    let status = ffmpeg::start_session(app.clone(), state, request.clone()).await?;
    let _ = transcript::start_session(
        app,
        asr_state,
        request.mic_id,
        status
            .destination
            .as_ref()
            .map(|dest| dest.label.clone())
            .unwrap_or_else(|| "StageBadger".to_string()),
        status.started_at_ms.unwrap_or_else(ffmpeg::now_millis),
    )
    .await
    .map_err(|err| {
        eprintln!("ASR start failed: {}", err);
        err
    });
    Ok(status)
}

#[tauri::command]
async fn start_recording(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    asr_state: tauri::State<'_, Arc<StdMutex<transcript::AsrRuntime>>>,
    camera_id: String,
    video_feeds: Option<types::VideoFeedSelection>,
    mic_id: String,
    recording: RecordingProfile,
    encoder: EncoderProfile,
    overlays: Vec<OverlayItem>,
    depth_of_field: bool,
) -> Result<SessionStatus, String> {
    let request = LiveSessionRequest {
        destination: DestinationConfig {
            kind: DestinationKind::RecordOnly,
            label: "Local recording".to_string(),
            manual_destination_id: None,
            rtmp_url: None,
            stream_key: None,
            broadcast_id: None,
            stream_id: None,
            live_chat_id: None,
        },
        recording,
        encoder,
        camera_id,
        video_feeds,
        mic_id: mic_id.clone(),
        overlays,
        depth_of_field,
        audio_filters: types::AudioFilters::default(),
        video_correction: types::VideoCorrection::default(),
    };

    let status = ffmpeg::start_session(app.clone(), state, request).await?;
    let _ = transcript::start_session(
        app,
        asr_state,
        mic_id,
        "Local recording".to_string(),
        status.started_at_ms.unwrap_or_else(ffmpeg::now_millis),
    )
    .await
    .map_err(|err| {
        eprintln!("ASR start failed: {}", err);
        err
    });
    Ok(status)
}

#[tauri::command]
async fn stop_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    asr_state: tauri::State<'_, Arc<StdMutex<transcript::AsrRuntime>>>,
    compact_after_stop: Option<bool>,
) -> Result<SessionStatus, String> {
    let status = ffmpeg::stop_session(app.clone(), state, compact_after_stop.unwrap_or(true)).await?;
    let _ = transcript::stop_session(asr_state).await;

    let final_media_path = status.compacted_path.clone().or_else(|| status.recording_path.clone());
    let mut final_status = status.clone();

    if let Some(media_path) = final_media_path {
        match transcript::process_media_to_transcript(
            std::path::Path::new(&media_path),
            status
                .destination
                .as_ref()
                .map(|dest| dest.label.clone())
                .unwrap_or_else(|| "StageBadger".to_string()),
            None,
            status.started_at_ms.unwrap_or_else(ffmpeg::now_millis),
        )
        .await
        {
            Ok(mut document) => {
                if let Err(err) =
                    transcript::write_transcript_artifacts(std::path::Path::new(&media_path), &mut document)
                {
                    let message = format!("Transcript finalization failed: {}", err);
                    final_status.error = Some(message.clone());
                    let _ = app.emit("system_log", message);
                } else {
                    let _ = app.emit("transcript_update", document);
                }
            }
            Err(err) => {
                let message = format!("Transcript finalization failed: {}", err);
                final_status.error = Some(message.clone());
                let _ = app.emit("system_log", message);
            }
        }
    }

    Ok(final_status)
}

#[tauri::command]
async fn get_session_status(state: tauri::State<'_, Arc<Mutex<StreamState>>>) -> Result<SessionStatus, String> {
    let st = state.lock().await;
    Ok(ffmpeg::session_status(&st))
}

#[tauri::command]
async fn add_overlay_asset(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    source_path: String,
    name: String,
) -> Result<OverlayStatus, String> {
    let source = std::path::Path::new(&source_path);
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .ok_or_else(|| "Overlay asset needs a file extension".to_string())?;
    if !matches!(extension.as_str(), "png" | "svg" | "webp") {
        return Err("Overlay assets must be transparent PNG, SVG, or WebP files".to_string());
    }

    let overlays_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("App data directory unavailable: {}", e))?
        .join("overlays");
    std::fs::create_dir_all(&overlays_dir).map_err(|e| format!("Failed to create overlay directory: {}", e))?;

    let id = format!("overlay-{}", ffmpeg::now_millis());
    let target = overlays_dir.join(format!("{}.{}", id, extension));
    std::fs::copy(source, &target).map_err(|e| format!("Failed to import overlay: {}", e))?;

    let mut st = state.lock().await;
    let overlay = OverlayItem {
        id,
        name,
        source_path: Some(source_path),
        asset_path: target.display().to_string(),
        x: 0.5,
        y: 0.5,
        scale: 1.0,
        opacity: 1.0,
        z_index: st.overlays.len() as i32 + 1,
        visible: true,
    };
    st.overlays.push(overlay);
    let status = OverlayStatus {
        overlays: st.overlays.clone(),
        message: "Overlay asset imported".to_string(),
    };
    let _ = app.emit("overlay_status", status.clone());
    Ok(status)
}

#[tauri::command]
async fn set_overlay_state(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    overlay: OverlayItem,
) -> Result<OverlayStatus, String> {
    let mut st = state.lock().await;
    if let Some(existing) = st.overlays.iter_mut().find(|item| item.id == overlay.id) {
        *existing = overlay;
    } else {
        st.overlays.push(overlay);
    }

    let status = OverlayStatus {
        overlays: st.overlays.clone(),
        message: "Overlay state updated".to_string(),
    };
    let _ = app.emit("overlay_status", status.clone());
    Ok(status)
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
/// and starts the event loop. ASR is attached to each session start rather
/// than running continuously at app launch.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = Arc::new(Mutex::new(StreamState::new()));
    let asr_state = Arc::new(StdMutex::new(transcript::AsrRuntime::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            #[cfg(desktop)]
            {
                let menu = tauri::menu::Menu::default(app.handle())?;
                app.set_menu(menu)?;
            }

            chat::initialize_chat_overlay();

            Ok(())
        })
        .manage(app_state)
        .manage(asr_state)
        .invoke_handler(tauri::generate_handler![
            get_av_devices,
            start_stream,
            stop_stream,
            check_moe_mount,
            connect_youtube,
            open_youtube_live_control_room,
            load_manual_destinations,
            save_manual_destination,
            delete_manual_destination,
            test_rtmp_destination,
            create_broadcast,
            start_live_session,
            start_recording,
            stop_session,
            add_overlay_asset,
            set_overlay_state,
            get_session_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

//! # FFmpeg Process Supervisor
//!
//! This module owns the entire FFmpeg lifecycle: device enumeration, argument construction,
//! process spawning, and teardown. It never touches raw media frames — FFmpeg does that work.
//!
//! ## Design Notes
//!
//! - Device enumeration uses `ffmpeg -list_devices` and parses stderr (FFmpeg writes device
//!   listings to stderr, not stdout).
//! - The filter graph is built dynamically based on whether a PNG overlay is provided.
//! - Output uses either plain FLV or the `tee` muxer for simultaneous stream + local recording.

use crate::types::{
    AudioFilters, DestinationKind, EncoderProfile, FfmpegTelemetry, LiveSessionRequest, PipPosition, RecordingStatus,
    SessionPhase, SessionStatus, StreamStatus, VideoCorrection, VideoEngineStatus, VideoFeedSelection, VideoSource,
    VideoSourceKind,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Holds the running FFmpeg child process, if any.
///
/// Wrapped in `Arc<Mutex<_>>` and managed as Tauri state so that
/// `start_stream` and `stop_stream` can coordinate safely.
pub struct StreamState {
    pub process: Option<Child>,
    pub phase: SessionPhase,
    pub destination: Option<crate::types::DestinationConfig>,
    pub recording_path: Option<PathBuf>,
    pub compacted_path: Option<PathBuf>,
    pub started_at: Option<Instant>,
    pub started_at_ms: Option<u64>,
    pub last_telemetry: FfmpegTelemetry,
    pub video_engine: VideoEngineStatus,
    pub overlays: Vec<crate::types::OverlayItem>,
    pub error: Option<String>,
}

impl StreamState {
    /// Create a new idle state with no running process.
    pub fn new() -> Self {
        Self {
            process: None,
            phase: SessionPhase::Idle,
            destination: None,
            recording_path: None,
            compacted_path: None,
            started_at: None,
            started_at_ms: None,
            last_telemetry: FfmpegTelemetry::default(),
            video_engine: VideoEngineStatus::default(),
            overlays: Vec::new(),
            error: None,
        }
    }
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

/// Available audio and video capture devices on this system.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AvDevices {
    pub video: Vec<String>,
    pub audio: Vec<String>,
    pub video_sources: Vec<VideoSource>,
}

fn video_source_kind(label: &str) -> VideoSourceKind {
    let lower = label.to_ascii_lowercase();
    if lower.contains("capture screen")
        || lower.contains("screen")
        || lower.contains("display")
        || lower.contains("desktop")
    {
        VideoSourceKind::Screen
    } else {
        VideoSourceKind::Camera
    }
}

fn parse_indexed_device_line(line: &str) -> Option<(usize, String)> {
    let start = line.find("] [")?;
    let after_bracket = &line[start + 3..];
    let end_idx = after_bracket.find("] ")?;
    let index = after_bracket[..end_idx].parse::<usize>().ok()?;
    let name = after_bracket[end_idx + 2..].trim();
    if name.is_empty() {
        None
    } else {
        Some((index, name.to_string()))
    }
}

pub fn make_video_source(index: usize, label: &str) -> VideoSource {
    let kind = video_source_kind(label);
    let prefix = match kind {
        VideoSourceKind::Camera => "camera",
        VideoSourceKind::Screen => "screen",
    };
    VideoSource {
        id: format!("{}-{}", prefix, index),
        label: label.to_string(),
        kind,
        avfoundation_name: label.to_string(),
        index,
    }
}

/// Parse FFmpeg's AVFoundation device listing from stderr output.
///
/// FFmpeg writes device information to stderr in the format:
/// ```text
/// [AVFoundation indev @ 0x...] [0] MacBook Pro Camera
/// ```
///
/// This function extracts the device names (after the index bracket).
pub fn parse_device_listing(stderr: &str) -> AvDevices {
    let mut video_devices = Vec::new();
    let mut audio_devices = Vec::new();
    let mut video_sources = Vec::new();
    let mut is_video = true;

    for line in stderr.lines() {
        if line.contains("AVFoundation video devices:") {
            is_video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices:") {
            is_video = false;
            continue;
        }

        // Match lines like: [AVFoundation indev @ 0x...] [0] Device Name
        if let Some((index, name)) = parse_indexed_device_line(line) {
            if is_video {
                video_sources.push(make_video_source(index, &name));
                video_devices.push(name);
            } else {
                audio_devices.push(name);
            }
        }
    }

    AvDevices {
        video: video_devices,
        audio: audio_devices,
        video_sources,
    }
}

/// Enumerate available AV devices by invoking FFmpeg.
///
/// Runs `ffmpeg -f avfoundation -list_devices true -i ""` and parses the stderr output.
pub async fn get_devices() -> Result<AvDevices, String> {
    let output = std::process::Command::new("ffmpeg")
        .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(parse_device_listing(&stderr))
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn default_recording_dir() -> PathBuf {
    let moe_dir = Path::new("/Volumes/MOE/StageBadger/Recordings");
    if Path::new("/Volumes/MOE").exists() {
        return moe_dir.to_path_buf();
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Movies")
        .join("StageBadger")
}

pub fn timestamped_recording_path(base_dir: Option<&str>, prefix: &str, extension: &str) -> PathBuf {
    let directory = base_dir.map(PathBuf::from).unwrap_or_else(default_recording_dir);
    let safe_prefix = sanitize_filename(prefix);
    let stamp = now_millis();
    directory.join(format!(
        "{}-{}.{}",
        safe_prefix,
        stamp,
        extension.trim_start_matches('.')
    ))
}

pub fn sanitize_filename(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "stagebadger".to_string()
    } else {
        trimmed.to_lowercase()
    }
}

pub fn redact_secret(value: &str) -> String {
    if value.len() <= 8 {
        return "[redacted]".to_string();
    }
    format!("{}...[redacted]", &value[..4])
}

pub fn redact_url(value: &str) -> String {
    if let Some((prefix, secret)) = value.rsplit_once('/') {
        if secret.len() > 8 {
            return format!("{}/{}", prefix, redact_secret(secret));
        }
    }
    value.replace("access_token=", "access_token=[redacted]")
}

pub fn redact_ffmpeg_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| {
            if arg.contains("rtmp://") || arg.contains("rtmps://") {
                redact_url(arg)
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn destination_output_url(destination: &crate::types::DestinationConfig) -> Result<Option<String>, String> {
    match destination.kind {
        DestinationKind::RecordOnly => Ok(None),
        DestinationKind::ManualRtmp | DestinationKind::YoutubeOauth | DestinationKind::YoutubeRtmps => {
            let base = destination
                .rtmp_url
                .as_deref()
                .ok_or_else(|| "RTMP ingest URL is missing".to_string())?;
            let key = destination
                .stream_key
                .as_deref()
                .ok_or_else(|| "Stream key is missing".to_string())?;
            Ok(Some(format!("{}{}", base, key)))
        }
    }
}

pub fn parse_ffmpeg_telemetry_line(line: &str, telemetry: &mut FfmpegTelemetry) -> bool {
    let mut changed = false;
    let normalized = line.replace("= ", "=");

    for part in normalized.split_whitespace() {
        if let Some(value) = part.strip_prefix("frame=") {
            if let Ok(frame) = value.trim().parse::<u64>() {
                telemetry.frame = Some(frame);
                changed = true;
            }
        } else if let Some(value) = part.strip_prefix("fps=") {
            if let Ok(fps) = value.trim().parse::<f32>() {
                telemetry.fps = Some(fps);
                changed = true;
            }
        } else if let Some(value) = part.strip_prefix("bitrate=") {
            let numeric = value.trim_end_matches("kbits/s").trim_end_matches("kb/s");
            if let Ok(kbps) = numeric.parse::<f32>() {
                telemetry.bitrate_kbps = Some(kbps);
                changed = true;
            }
        } else if let Some(value) = part.strip_prefix("speed=") {
            let numeric = value.trim_end_matches('x');
            if let Ok(speed) = numeric.parse::<f32>() {
                telemetry.speed = Some(speed);
                changed = true;
            }
        } else if let Some(value) = part.strip_prefix("drop=") {
            if let Ok(dropped) = value.trim().parse::<u64>() {
                telemetry.dropped_frames = dropped;
                changed = true;
            }
        }
    }

    let lower = line.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("invalid") || lower.contains("failed") {
        telemetry.errors += 1;
        changed = true;
    }

    if changed {
        telemetry.last_line = Some(redact_url(line));
    }

    changed
}

pub fn session_status(state: &StreamState) -> SessionStatus {
    let duration_ms = state
        .started_at
        .map(|started| started.elapsed().as_millis() as u64)
        .unwrap_or(0);
    let bytes_written = state
        .recording_path
        .as_ref()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let compacted_bytes = state
        .compacted_path
        .as_ref()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    let bitrate_kbps = if duration_ms > 0 && bytes_written > 0 {
        Some((bytes_written as f32 * 8.0) / duration_ms as f32)
    } else {
        state.last_telemetry.bitrate_kbps
    };

    SessionStatus {
        phase: state.phase.clone(),
        destination: state.destination.clone(),
        recording_path: state.recording_path.as_ref().map(|path| path.display().to_string()),
        compacted_path: state.compacted_path.as_ref().map(|path| path.display().to_string()),
        started_at_ms: state.started_at_ms,
        duration_ms,
        bytes_written,
        compacted_bytes,
        bitrate_kbps,
        telemetry: state.last_telemetry.clone(),
        video_engine: state.video_engine.clone(),
        overlays: state.overlays.clone(),
        error: state.error.clone(),
    }
}

/// Build the FFmpeg `-filter_complex` string.
///
/// The filter graph layers:
/// 1. (Optional) PNG overlay composited at top-right with 20px margin
/// 2. Chat text overlay at top-left (reloaded from file every frame)
/// 3. ASR caption overlay at bottom-center (reloaded from file every frame)
/// 4. (Optional) Boxblur/Gblur applied prior to overlays if background blur is toggled
///
/// Returns the filter string and whether a PNG overlay input is included.
pub fn build_filter_graph(overlay_path: &str, blur_background: bool) -> (String, bool) {
    let drawtext_chat = "drawtext=textfile='/tmp/stagebadger_chat.txt'\
        :reload=1\
        :fontfile='/System/Library/Fonts/Helvetica.ttc'\
        :fontcolor=white:fontsize=24\
        :x=10:y=10\
        :box=1:boxcolor=black@0.5";

    let drawtext_asr = "drawtext=textfile='/tmp/stagebadger_asr.txt'\
        :reload=1\
        :fontfile='/System/Library/Fonts/Helvetica.ttc'\
        :fontcolor=yellow:fontsize=32\
        :x=(w-text_w)/2:y=h-th-20\
        :box=1:boxcolor=black@0.5";

    if !overlay_path.is_empty() && std::path::Path::new(overlay_path).exists() {
        // scale2ref scales the overlay [1:v] to match the camera video dimensions,
        // then overlay at 0,0 for full-frame coverage preserving alpha transparency
        let filter = if blur_background {
            format!(
                "[0:v]gblur=sigma=10:steps=2[base];[1:v][base]scale2ref[ovr][ref];[ref][ovr]overlay=0:0:format=auto[v1];[v1]{},{}[v_out]",
                drawtext_chat, drawtext_asr
            )
        } else {
            format!(
                "[1:v][0:v]scale2ref[ovr][ref];[ref][ovr]overlay=0:0:format=auto[v1];[v1]{},{}[v_out]",
                drawtext_chat, drawtext_asr
            )
        };
        (filter, true)
    } else {
        let filter = if blur_background {
            format!("[0:v]gblur=sigma=10:steps=2,{},{}[v_out]", drawtext_chat, drawtext_asr)
        } else {
            format!("{},{}[v_out]", drawtext_chat, drawtext_asr)
        };
        (filter, false)
    }
}

fn normalize_video_filter(input: &str, output: &str, kind: &VideoSourceKind, width: u32, height: u32) -> String {
    match kind {
        VideoSourceKind::Camera => format!(
            "{}scale={}:{}:force_original_aspect_ratio=increase,crop={}:{},setsar=1{}",
            input, width, height, width, height, output
        ),
        VideoSourceKind::Screen => format!(
            "{}scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,setsar=1{}",
            input, width, height, width, height, output
        ),
    }
}

fn pip_overlay_xy(position: &PipPosition, _margin: u32) -> (&'static str, &'static str) {
    match position {
        PipPosition::BottomRight => ("W-w-24", "H-h-24"),
        PipPosition::BottomLeft => ("24", "H-h-24"),
        PipPosition::TopRight => ("W-w-24", "24"),
        PipPosition::TopLeft => ("24", "24"),
    }
}

/// Build the program feed filter graph before PNG overlays, chat text, and ASR captions.
pub fn build_composed_filter_graph(
    feeds: &VideoFeedSelection,
    overlay_path: &str,
    encoder: &EncoderProfile,
    blur_background: bool,
) -> (String, bool) {
    let drawtext_chat = "drawtext=textfile='/tmp/stagebadger_chat.txt'\
        :reload=1\
        :fontfile='/System/Library/Fonts/Helvetica.ttc'\
        :fontcolor=white:fontsize=24\
        :x=10:y=10\
        :box=1:boxcolor=black@0.5";

    let drawtext_asr = "drawtext=textfile='/tmp/stagebadger_asr.txt'\
        :reload=1\
        :fontfile='/System/Library/Fonts/Helvetica.ttc'\
        :fontcolor=yellow:fontsize=32\
        :x=(w-text_w)/2:y=h-th-20\
        :box=1:boxcolor=black@0.5";

    let has_overlay = !overlay_path.is_empty() && std::path::Path::new(overlay_path).exists();
    let mut parts = vec![normalize_video_filter(
        "[0:v]",
        "[program0]",
        &feeds.primary.kind,
        encoder.width,
        encoder.height,
    )];
    let mut current = "program0".to_string();

    if blur_background {
        parts.push(format!("[{}]gblur=sigma=10:steps=2[program_blur]", current));
        current = "program_blur".to_string();
    }

    if feeds.layout.pip_enabled {
        if let Some(pip) = feeds.pip.as_ref() {
            let pip_width = ((encoder.width as f32 * feeds.layout.pip_size_percent.clamp(18.0, 35.0) / 100.0).round()
                as u32)
                .max(2);
            let pip_height = ((pip_width as f32 * 9.0 / 16.0).round() as u32).max(2);
            parts.push(normalize_video_filter(
                "[1:v]", "[pip0]", &pip.kind, pip_width, pip_height,
            ));
            let (x, y) = pip_overlay_xy(&feeds.layout.pip_position, 24);
            parts.push(format!(
                "[{}][pip0]overlay=x={}:y={}:format=auto[program_pip]",
                current, x, y
            ));
            current = "program_pip".to_string();
        }
    }

    if has_overlay {
        let overlay_input_index = if feeds.layout.pip_enabled && feeds.pip.is_some() {
            2
        } else {
            1
        };
        parts.push(format!(
            "[{}:v][{}]scale2ref[program_ovr][program_ref];[program_ref][program_ovr]overlay=0:0:format=auto[program_overlay]",
            overlay_input_index, current
        ));
        current = "program_overlay".to_string();
    }

    parts.push(format!("[{}]{},{}[v_out]", current, drawtext_chat, drawtext_asr));
    (parts.join(";"), has_overlay)
}

pub fn validate_video_feeds(feeds: &VideoFeedSelection) -> Result<(), String> {
    if feeds.layout.pip_enabled {
        let pip = feeds
            .pip
            .as_ref()
            .ok_or_else(|| "In-screen feed is enabled but no PiP source was selected".to_string())?;
        if feeds.primary.id == pip.id || feeds.primary.avfoundation_name == pip.avfoundation_name {
            return Err("Program feed and in-screen feed must be different sources".to_string());
        }
    }
    Ok(())
}

fn legacy_video_feeds(camera_id: &str) -> VideoFeedSelection {
    VideoFeedSelection {
        primary: make_video_source(0, camera_id),
        pip: None,
        layout: crate::types::VideoFeedLayout::default(),
    }
}

fn resolved_video_feeds<'a>(camera_id: &str, video_feeds: Option<&'a VideoFeedSelection>) -> VideoFeedSelection {
    video_feeds.cloned().unwrap_or_else(|| legacy_video_feeds(camera_id))
}

/// Build the complete FFmpeg argument vector for a broadcast session.
///
/// This is a pure function (no side effects) that constructs the args
/// based on the user's configuration. Extracted for testability.
pub fn build_ffmpeg_args(
    cam: &str,
    mic: &str,
    overlay_path: &str,
    server_url: &str,
    key: &str,
    enable_recording: bool,
    blur_background: bool,
) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "info".to_string(),
        "-f".to_string(),
        "avfoundation".to_string(),
        "-r".to_string(),
        "30".to_string(),
        "-i".to_string(),
        format!("{}:{}", cam, mic),
    ];

    let (filter_complex, has_overlay) = build_filter_graph(overlay_path, blur_background);

    if has_overlay {
        args.push("-i".to_string());
        args.push(overlay_path.to_string());
    }

    args.extend([
        "-filter_complex".to_string(),
        filter_complex,
        "-c:v".to_string(),
        "libx264".to_string(),
        "-preset".to_string(),
        "veryfast".to_string(),
        "-b:v".to_string(),
        "3000k".to_string(),
        "-maxrate".to_string(),
        "3000k".to_string(),
        "-bufsize".to_string(),
        "6000k".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-g".to_string(),
        "60".to_string(),
        "-c:a".to_string(),
        "aac".to_string(),
        "-b:a".to_string(),
        "160k".to_string(),
        "-ar".to_string(),
        "44100".to_string(),
    ]);

    let output_url = format!("{}{}", server_url, key);
    if enable_recording {
        // Ensure the recording directory exists
        let _ = fs::create_dir_all("/Volumes/MOE/Recordings");
        args.extend([
            "-f".to_string(),
            "tee".to_string(),
            "-map".to_string(),
            "[v_out]".to_string(),
            "-map".to_string(),
            "0:a".to_string(),
            format!(
                "[f=flv:onfail=ignore]{}|[f=mp4:onfail=ignore]/Volumes/MOE/Recordings/live_capture.mp4",
                output_url
            ),
        ]);
    } else {
        args.extend([
            "-f".to_string(),
            "flv".to_string(),
            "-map".to_string(),
            "[v_out]".to_string(),
            "-map".to_string(),
            "0:a".to_string(),
            output_url,
        ]);
    }

    args
}

pub fn build_session_ffmpeg_args(
    cam: &str,
    mic: &str,
    overlay_path: &str,
    output_url: Option<&str>,
    recording_path: Option<&Path>,
    encoder: &EncoderProfile,
    blur_background: bool,
    audio_filters: &AudioFilters,
    video_correction: &VideoCorrection,
) -> Vec<String> {
    let feeds = legacy_video_feeds(cam);
    build_session_ffmpeg_args_for_feeds(
        &feeds,
        mic,
        overlay_path,
        output_url,
        recording_path,
        encoder,
        blur_background,
        audio_filters,
        video_correction,
    )
}

pub fn build_session_ffmpeg_args_for_feeds(
    feeds: &VideoFeedSelection,
    mic: &str,
    overlay_path: &str,
    output_url: Option<&str>,
    recording_path: Option<&Path>,
    encoder: &EncoderProfile,
    blur_background: bool,
    audio_filters: &AudioFilters,
    video_correction: &VideoCorrection,
) -> Vec<String> {
    let pip_enabled = feeds.layout.pip_enabled && feeds.pip.is_some();
    let (filter_complex, has_overlay) = build_composed_filter_graph(feeds, overlay_path, encoder, blur_background);

    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "info".to_string(),
        "-stats".to_string(),
        "-f".to_string(),
        "avfoundation".to_string(),
        "-framerate".to_string(),
        encoder.fps.to_string(),
        "-i".to_string(),
        format!("{}:", feeds.primary.avfoundation_name),
    ];

    if pip_enabled {
        if let Some(pip) = feeds.pip.as_ref() {
            args.extend([
                "-f".to_string(),
                "avfoundation".to_string(),
                "-framerate".to_string(),
                encoder.fps.to_string(),
                "-i".to_string(),
                format!("{}:", pip.avfoundation_name),
            ]);
        }
    }

    if has_overlay {
        args.push("-i".to_string());
        args.push(overlay_path.to_string());
    }

    let audio_input_index = 1 + usize::from(pip_enabled) + usize::from(has_overlay);
    args.extend([
        "-f".to_string(),
        "avfoundation".to_string(),
        "-i".to_string(),
        format!(":{}", mic),
    ]);

    args.extend([
        "-filter_complex".to_string(),
        filter_complex,
        "-map".to_string(),
        "[v_out]".to_string(),
        "-map".to_string(),
        format!("{}:a", audio_input_index),
    ]);

    // Build audio filter chain from enabled AudioFilters
    let mut af_parts: Vec<String> = Vec::new();
    if audio_filters.noise_suppression {
        let nr = (audio_filters.noise_suppression_level * 97.0 + 3.0).clamp(3.0, 97.0);
        af_parts.push(format!("afftdn=nr={:.0}:nf=-25", nr));
    }
    if audio_filters.noise_gate {
        let threshold = audio_filters.noise_gate_threshold_db.clamp(-80.0, 0.0);
        af_parts.push(format!("agate=threshold={}:ratio=4:attack=5:release=50", threshold));
    }
    if audio_filters.compressor {
        af_parts.push("acompressor=threshold=-20dB:ratio=4:attack=5:release=50:makeup=2".to_string());
    }
    if (audio_filters.gain_db - 0.0).abs() > 0.1 {
        af_parts.push(format!("volume={}dB", audio_filters.gain_db));
    }
    if !af_parts.is_empty() {
        args.extend(["-af".to_string(), af_parts.join(",")]);
        println!("FFmpeg: Audio filters → {}", af_parts.join(","));
    }

    // Video color correction via eq filter (applied in filter_complex above, but we can add as a second -vf)
    // Note: Since we already use -filter_complex, eq must be injected there. We handle this by
    // inserting it just before [v_out] in the filter graph. For now, log the correction params.
    if video_correction.enabled {
        println!(
            "FFmpeg: Video correction → brightness={:.2} contrast={:.2} saturation={:.2} gamma={:.2}",
            video_correction.brightness, video_correction.contrast, video_correction.saturation, video_correction.gamma
        );
    }

    args.extend([
        "-c:v".to_string(),
        "libx264".to_string(),
        "-preset".to_string(),
        "veryfast".to_string(),
        "-b:v".to_string(),
        format!("{}k", encoder.video_bitrate_kbps),
        "-maxrate".to_string(),
        format!("{}k", encoder.video_bitrate_kbps),
        "-bufsize".to_string(),
        format!("{}k", encoder.video_bitrate_kbps * 2),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-g".to_string(),
        (encoder.fps * 2).to_string(),
        "-c:a".to_string(),
        "aac".to_string(),
        "-b:a".to_string(),
        format!("{}k", encoder.audio_bitrate_kbps),
        "-ar".to_string(),
        "48000".to_string(),
    ]);

    match (output_url, recording_path) {
        (Some(url), Some(path)) => {
            args.extend([
                "-f".to_string(),
                "tee".to_string(),
                format!(
                    "[f=flv:onfail=abort]{}|[f=mp4:movflags=+frag_keyframe+empty_moov+default_base_moof:onfail=ignore]{}",
                    url,
                    path.display()
                ),
            ]);
        }
        (Some(url), None) => {
            args.extend(["-f".to_string(), "flv".to_string(), url.to_string()]);
        }
        (None, Some(path)) => {
            args.extend([
                "-f".to_string(),
                "mp4".to_string(),
                "-movflags".to_string(),
                "+frag_keyframe+empty_moov+default_base_moof".to_string(),
                path.display().to_string(),
            ]);
        }
        (None, None) => {
            args.extend(["-f".to_string(), "null".to_string(), "-".to_string()]);
        }
    }

    args
}

/// Start a broadcast session.
///
/// Initializes overlay text files, builds the FFmpeg argument vector,
/// and spawns the child process. ASR and chat workers are started
/// independently on app launch (see lib.rs setup hook).
pub async fn start(
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    server_url: String,
    key: String,
    cam: String,
    mic: String,
    enable_recording: bool,
    overlay_path: String,
    blur_background: bool,
) -> Result<(), String> {
    let mut st = state.lock().await;
    if st.process.is_some() {
        return Err("Stream is already running".into());
    }

    // Initialize overlay files with placeholder content
    let _ = fs::write("/tmp/stagebadger_chat.txt", "Live Chat Initializing...\n");
    let _ = fs::write("/tmp/stagebadger_asr.txt", "ASR Standby...");

    let args = build_ffmpeg_args(
        &cam,
        &mic,
        &overlay_path,
        &server_url,
        &key,
        enable_recording,
        blur_background,
    );

    println!("FFmpeg: ====== BROADCAST START ======");
    println!("FFmpeg: Server: {}", server_url);
    println!("FFmpeg: Camera: {}, Mic: {}", cam, mic);
    println!(
        "FFmpeg: Overlay: '{}' (exists={})",
        overlay_path,
        std::path::Path::new(&overlay_path).exists()
    );
    println!("FFmpeg: Recording: {}, Blur: {}", enable_recording, blur_background);
    println!("FFmpeg: Args: {:?}", redact_ffmpeg_args(&args));

    let child = Command::new("ffmpeg").args(&args).spawn().map_err(|e| {
        println!("FFmpeg: CRITICAL — spawn failed: {}", e);
        format!("Failed to start FFmpeg: {}", e)
    })?;

    println!("FFmpeg: Process spawned successfully (PID: {:?})", child.id());
    st.phase = SessionPhase::Live;
    st.started_at = Some(Instant::now());
    st.started_at_ms = Some(now_millis());
    st.error = None;
    st.process = Some(child);

    Ok(())
}

pub async fn start_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    request: LiveSessionRequest,
) -> Result<SessionStatus, String> {
    let state_for_telemetry = Arc::clone(state.inner());
    let output_url = destination_output_url(&request.destination)?;
    let should_record = request.recording.enabled || request.destination.kind != DestinationKind::RecordOnly;
    let recording_path = if should_record {
        let path = timestamped_recording_path(
            request.recording.directory.as_deref(),
            &request.recording.filename_prefix,
            "mp4",
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create recording directory: {}", e))?;
        }
        Some(path)
    } else {
        None
    };
    let primary_overlay = request
        .overlays
        .iter()
        .filter(|overlay| overlay.visible)
        .max_by_key(|overlay| overlay.z_index)
        .map(|overlay| overlay.asset_path.as_str())
        .unwrap_or("");

    let feeds = resolved_video_feeds(&request.camera_id, request.video_feeds.as_ref());
    validate_video_feeds(&feeds)?;

    let args = build_session_ffmpeg_args_for_feeds(
        &feeds,
        &request.mic_id,
        primary_overlay,
        output_url.as_deref(),
        recording_path.as_deref(),
        &request.encoder,
        request.depth_of_field,
        &request.audio_filters,
        &request.video_correction,
    );

    let mut st = state.lock().await;
    if st.process.is_some() {
        return Err("A StageBadger session is already running".into());
    }

    st.phase = if request.destination.kind == DestinationKind::RecordOnly {
        SessionPhase::Recording
    } else {
        SessionPhase::Connecting
    };
    st.destination = Some(request.destination.clone());
    st.recording_path = recording_path.clone();
    st.compacted_path = None;
    st.started_at = Some(Instant::now());
    st.started_at_ms = Some(now_millis());
    st.last_telemetry = FfmpegTelemetry::default();
    st.video_engine = VideoEngineStatus {
        engine: if request.depth_of_field {
            "vision-coreimage".to_string()
        } else {
            "direct-ffmpeg".to_string()
        },
        depth_of_field: request.depth_of_field,
        fallback_active: !request.depth_of_field,
        queue_depth: 0,
        dropped_frames: 0,
        message: if request.depth_of_field {
            "Native macOS person-segmentation compositor requested; direct FFmpeg capture remains available as fallback"
                .to_string()
        } else {
            "Direct FFmpeg capture path active".to_string()
        },
    };
    st.overlays = request.overlays.clone();
    st.error = None;

    let _ = app.emit(
        "stream_status",
        StreamStatus {
            phase: st.phase.clone(),
            destination: request.destination.label.clone(),
            message: "Starting FFmpeg pipeline".to_string(),
        },
    );
    let _ = app.emit("video_engine_status", st.video_engine.clone());

    println!("FFmpeg: ====== SESSION START ======");
    println!("FFmpeg: Destination: {}", request.destination.label);
    println!(
        "FFmpeg: Program: {} ({:?}), PiP: {:?}, Mic: {}",
        feeds.primary.avfoundation_name,
        feeds.primary.kind,
        feeds.pip.as_ref().map(|pip| pip.avfoundation_name.as_str()),
        request.mic_id
    );
    println!("FFmpeg: Recording: {:?}", recording_path);
    println!("FFmpeg: Args: {:?}", redact_ffmpeg_args(&args));

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            st.phase = SessionPhase::Error;
            st.error = Some(format!("Failed to start FFmpeg: {}", e));
            format!("Failed to start FFmpeg: {}", e)
        })?;

    if let Some(stderr) = child.stderr.take() {
        let app_for_telemetry = app.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            let mut telemetry = FfmpegTelemetry::default();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if parse_ffmpeg_telemetry_line(&line, &mut telemetry) {
                            if let Ok(mut state) = state_for_telemetry.try_lock() {
                                state.last_telemetry = telemetry.clone();
                            }
                            let _ = app_for_telemetry.emit("ffmpeg_telemetry", telemetry.clone());
                            let _ = app_for_telemetry.emit(
                                "stream_status",
                                StreamStatus {
                                    phase: SessionPhase::Live,
                                    destination: "FFmpeg".to_string(),
                                    message: telemetry
                                        .last_line
                                        .clone()
                                        .unwrap_or_else(|| "FFmpeg telemetry update".to_string()),
                                },
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        });
    }

    st.phase = if request.destination.kind == DestinationKind::RecordOnly {
        SessionPhase::Recording
    } else {
        SessionPhase::Live
    };
    st.process = Some(child);

    let status = session_status(&st);
    let _ = app.emit(
        "recording_status",
        RecordingStatus {
            path: status.recording_path.clone(),
            compacted_path: status.compacted_path.clone(),
            duration_ms: status.duration_ms,
            bytes_written: status.bytes_written,
            bitrate_kbps: status.bitrate_kbps,
            compacted_bytes: status.compacted_bytes,
        },
    );
    let _ = app.emit(
        "stream_status",
        StreamStatus {
            phase: st.phase.clone(),
            destination: request.destination.label,
            message: "Session running".to_string(),
        },
    );

    Ok(status)
}

/// Stop the active broadcast by killing the FFmpeg child process.
///
/// This is safe to call even if no stream is running (it will return `Ok(())`).
pub async fn stop(state: tauri::State<'_, Arc<Mutex<StreamState>>>) -> Result<(), String> {
    let mut st = state.lock().await;
    st.phase = SessionPhase::Stopping;
    if let Some(mut child) = st.process.take() {
        let _ = child.kill().await;
        match child.wait().await {
            Ok(status) => {
                st.last_telemetry.exit_reason = Some(status.to_string());
            }
            Err(err) => {
                st.last_telemetry.exit_reason = Some(format!("wait failed: {}", err));
            }
        }
    }
    st.phase = SessionPhase::Idle;
    Ok(())
}

pub async fn stop_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<StreamState>>>,
    compact_after_stop: bool,
) -> Result<SessionStatus, String> {
    let recording_to_compact = {
        let mut st = state.lock().await;
        st.phase = SessionPhase::Stopping;
        let _ = app.emit(
            "stream_status",
            StreamStatus {
                phase: SessionPhase::Stopping,
                destination: st
                    .destination
                    .as_ref()
                    .map(|dest| dest.label.clone())
                    .unwrap_or_else(|| "StageBadger".to_string()),
                message: "Stopping session".to_string(),
            },
        );

        if let Some(mut child) = st.process.take() {
            let _ = child.kill().await;
            match child.wait().await {
                Ok(status) => {
                    st.last_telemetry.exit_reason = Some(status.to_string());
                }
                Err(err) => {
                    st.last_telemetry.exit_reason = Some(format!("wait failed: {}", err));
                }
            }
        }

        st.recording_path
            .clone()
            .filter(|path| compact_after_stop && path.exists())
    };

    let compacted_path = if let Some(path) = recording_to_compact {
        compact_recording(&path).await.ok()
    } else {
        None
    };

    let mut st = state.lock().await;
    if let Some(path) = compacted_path {
        st.compacted_path = Some(path);
    }
    st.phase = SessionPhase::Idle;
    st.started_at = None;

    let status = session_status(&st);
    let _ = app.emit(
        "recording_status",
        RecordingStatus {
            path: status.recording_path.clone(),
            compacted_path: status.compacted_path.clone(),
            duration_ms: status.duration_ms,
            bytes_written: status.bytes_written,
            bitrate_kbps: status.bitrate_kbps,
            compacted_bytes: status.compacted_bytes,
        },
    );
    let _ = app.emit(
        "stream_status",
        StreamStatus {
            phase: SessionPhase::Idle,
            destination: status
                .destination
                .as_ref()
                .map(|dest| dest.label.clone())
                .unwrap_or_else(|| "StageBadger".to_string()),
            message: "Session stopped".to_string(),
        },
    );

    Ok(status)
}

pub async fn compact_recording(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("Recording does not exist: {}", path.display()));
    }

    let output = path.with_extension("compact.mov");
    let input_arg = path.display().to_string();
    let output_arg = output.display().to_string();
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &input_arg,
            "-c:v",
            "hevc_videotoolbox",
            "-tag:v",
            "hvc1",
            "-c:a",
            "copy",
            &output_arg,
        ])
        .status()
        .await
        .map_err(|e| format!("Failed to compact recording: {}", e))?;

    if !status.success() {
        return Err(format!("FFmpeg compact exited with {}", status));
    }

    let input_size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    let output_size = fs::metadata(&output).map_err(|e| e.to_string())?.len();
    if output_size == 0 || input_size == 0 {
        let _ = fs::remove_file(&output);
        return Err("Compacted recording did not verify".to_string());
    }

    Ok(output)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_device_listing_typical() {
        let stderr = r#"[AVFoundation indev @ 0x1395041a0] AVFoundation video devices:
[AVFoundation indev @ 0x1395041a0] [0] MacBook Pro Camera
[AVFoundation indev @ 0x1395041a0] [1] Capture screen 0
[AVFoundation indev @ 0x1395041a0] AVFoundation audio devices:
[AVFoundation indev @ 0x1395041a0] [0] MacBook Pro Microphone"#;

        let devices = parse_device_listing(stderr);
        assert_eq!(devices.video, vec!["MacBook Pro Camera", "Capture screen 0"]);
        assert_eq!(devices.audio, vec!["MacBook Pro Microphone"]);
        assert_eq!(devices.video_sources.len(), 2);
        assert_eq!(devices.video_sources[0].kind, VideoSourceKind::Camera);
        assert_eq!(devices.video_sources[1].kind, VideoSourceKind::Screen);
        assert_eq!(devices.video_sources[1].index, 1);
    }

    #[test]
    fn test_parse_device_listing_empty() {
        let stderr = "ffmpeg version 8.1\nsome unrelated output\n";
        let devices = parse_device_listing(stderr);
        assert!(devices.video.is_empty());
        assert!(devices.audio.is_empty());
    }

    #[test]
    fn test_parse_device_listing_multiple_screens() {
        let stderr = r#"[AVFoundation indev @ 0x123] AVFoundation video devices:
[AVFoundation indev @ 0x123] [0] FaceTime HD Camera
[AVFoundation indev @ 0x123] [1] Capture screen 0
[AVFoundation indev @ 0x123] [2] Capture screen 1
[AVFoundation indev @ 0x123] [3] Capture screen 2
[AVFoundation indev @ 0x123] AVFoundation audio devices:
[AVFoundation indev @ 0x123] [0] MacBook Pro Microphone
[AVFoundation indev @ 0x123] [1] USB Condenser Mic"#;

        let devices = parse_device_listing(stderr);
        assert_eq!(devices.video.len(), 4);
        assert_eq!(devices.audio.len(), 2);
        assert_eq!(devices.audio[1], "USB Condenser Mic");
        assert_eq!(
            devices
                .video_sources
                .iter()
                .filter(|source| source.kind == VideoSourceKind::Screen)
                .count(),
            3
        );
    }

    #[test]
    fn test_build_filter_graph_no_overlay() {
        let (filter, has_overlay) = build_filter_graph("", false);
        assert!(!has_overlay);
        assert!(filter.contains("drawtext="));
        assert!(filter.ends_with("[v_out]"));
        // Should NOT contain overlay filter
        assert!(!filter.contains("[0:v][1:v]overlay"));
    }

    #[test]
    fn test_build_filter_graph_with_nonexistent_overlay() {
        let (filter, has_overlay) = build_filter_graph("/tmp/nonexistent_file_xyz_123.png", false);
        assert!(!has_overlay);
        assert!(!filter.contains("overlay"));
    }

    #[test]
    fn test_build_ffmpeg_args_stream_only() {
        let args = build_ffmpeg_args(
            "MacBook Pro Camera",
            "MacBook Pro Microphone",
            "",
            "rtmp://a.rtmp.youtube.com/live2/",
            "test-key-123",
            false,
            false,
        );

        // Verify essential args are present
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&"avfoundation".to_string()));
        assert!(args.contains(&"flv".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.contains(&"veryfast".to_string()));

        // Verify the output URL is correctly constructed
        let output_url = "rtmp://a.rtmp.youtube.com/live2/test-key-123".to_string();
        assert!(args.contains(&output_url));

        // Should NOT contain tee muxer
        assert!(!args.contains(&"tee".to_string()));
    }

    #[test]
    fn test_build_ffmpeg_args_with_recording() {
        let args = build_ffmpeg_args(
            "MacBook Pro Camera",
            "MacBook Pro Microphone",
            "",
            "rtmp://a.rtmp.youtube.com/live2/",
            "test-key-123",
            true,
            false,
        );

        // Should use tee muxer
        assert!(args.contains(&"tee".to_string()));
        // Should NOT contain plain flv output
        let flv_count = args.iter().filter(|a| *a == "flv").count();
        assert_eq!(flv_count, 0);

        // Verify the tee output contains both destinations
        let tee_output = args.last().unwrap();
        assert!(tee_output.contains("[f=flv:onfail=ignore]"));
        assert!(tee_output.contains("[f=mp4:onfail=ignore]"));
        assert!(tee_output.contains("/Volumes/MOE/Recordings/live_capture.mp4"));
    }

    #[test]
    fn test_build_ffmpeg_args_input_format() {
        let args = build_ffmpeg_args("0", "0", "", "rtmp://test/", "key", false, false);

        // Verify the AV input is formatted as "camera:mic"
        assert!(args.contains(&"0:0".to_string()));
    }

    #[test]
    fn test_build_ffmpeg_args_encoding_params() {
        let args = build_ffmpeg_args("0", "0", "", "rtmp://test/", "key", false, false);

        // Verify critical encoding parameters
        assert!(args.contains(&"3000k".to_string())); // bitrate
        assert!(args.contains(&"6000k".to_string())); // bufsize
        assert!(args.contains(&"yuv420p".to_string())); // pixel format
        assert!(args.contains(&"60".to_string())); // GOP size
        assert!(args.contains(&"aac".to_string())); // audio codec
        assert!(args.contains(&"160k".to_string())); // audio bitrate
        assert!(args.contains(&"44100".to_string())); // sample rate
    }

    #[test]
    fn test_stream_state_default() {
        let state = StreamState::default();
        assert!(state.process.is_none());
    }

    #[test]
    fn test_av_devices_serde_roundtrip() {
        let devices = AvDevices {
            video: vec!["Camera A".into(), "Camera B".into()],
            audio: vec!["Mic 1".into()],
            video_sources: vec![make_video_source(0, "Camera A"), make_video_source(1, "Camera B")],
        };

        let json = serde_json::to_string(&devices).unwrap();
        let deserialized: AvDevices = serde_json::from_str(&json).unwrap();
        assert_eq!(devices, deserialized);
    }

    #[test]
    fn test_build_ffmpeg_args_map_contains_v_out() {
        let args = build_ffmpeg_args("0", "0", "", "rtmp://test/", "key", false, false);
        assert!(args.contains(&"[v_out]".to_string()));
    }

    #[test]
    fn test_parse_ffmpeg_telemetry_line() {
        let mut telemetry = FfmpegTelemetry::default();
        let changed = parse_ffmpeg_telemetry_line(
            "frame=123 fps=29.97 bitrate=5800.4kbits/s speed=1.01x drop=2",
            &mut telemetry,
        );
        assert!(changed);
        assert_eq!(telemetry.frame, Some(123));
        assert_eq!(telemetry.fps, Some(29.97));
        assert_eq!(telemetry.dropped_frames, 2);
        assert_eq!(telemetry.speed, Some(1.01));
    }

    #[test]
    fn test_build_session_ffmpeg_args_live_always_records() {
        let encoder = EncoderProfile::default();
        let recording = Path::new("/tmp/stagebadger-test.mp4");
        let args = build_session_ffmpeg_args(
            "0",
            "0",
            "",
            Some("rtmps://example/live/key"),
            Some(recording),
            &encoder,
            false,
            &AudioFilters::default(),
            &VideoCorrection::default(),
        );
        let tee_output = args.last().unwrap();
        assert!(args.contains(&"tee".to_string()));
        assert!(tee_output.contains("[f=flv:onfail=abort]"));
        assert!(tee_output.contains("/tmp/stagebadger-test.mp4"));
    }

    #[test]
    fn test_build_session_ffmpeg_args_primary_camera_maps_audio_from_last_input() {
        let encoder = EncoderProfile::default();
        let feeds = VideoFeedSelection {
            primary: make_video_source(0, "MacBook Pro Camera"),
            pip: None,
            layout: crate::types::VideoFeedLayout::default(),
        };
        let args = build_session_ffmpeg_args_for_feeds(
            &feeds,
            "MacBook Pro Microphone",
            "",
            None,
            Some(Path::new("/tmp/stagebadger-test.mp4")),
            &encoder,
            false,
            &AudioFilters::default(),
            &VideoCorrection::default(),
        );
        assert!(args.contains(&"MacBook Pro Camera:".to_string()));
        assert!(args.contains(&":MacBook Pro Microphone".to_string()));
        assert!(args.contains(&"1:a".to_string()));
    }

    #[test]
    fn test_build_session_ffmpeg_args_primary_screen_preserves_aspect() {
        let encoder = EncoderProfile::default();
        let feeds = VideoFeedSelection {
            primary: make_video_source(1, "Capture screen 0"),
            pip: None,
            layout: crate::types::VideoFeedLayout::default(),
        };
        let args = build_session_ffmpeg_args_for_feeds(
            &feeds,
            "Mic",
            "",
            None,
            None,
            &encoder,
            false,
            &AudioFilters::default(),
            &VideoCorrection::default(),
        );
        let filter = args
            .iter()
            .position(|arg| arg == "-filter_complex")
            .and_then(|index| args.get(index + 1))
            .unwrap();
        assert!(filter.contains("force_original_aspect_ratio=decrease"));
        assert!(filter.contains("pad=1920:1080"));
    }

    #[test]
    fn test_build_session_ffmpeg_args_screen_with_camera_pip() {
        let encoder = EncoderProfile::default();
        let feeds = VideoFeedSelection {
            primary: make_video_source(1, "Capture screen 0"),
            pip: Some(make_video_source(0, "MacBook Pro Camera")),
            layout: crate::types::VideoFeedLayout {
                pip_enabled: true,
                pip_position: PipPosition::BottomRight,
                pip_size_percent: 24.0,
            },
        };
        let args = build_session_ffmpeg_args_for_feeds(
            &feeds,
            "Mic",
            "",
            Some("rtmps://example/live/key"),
            Some(Path::new("/tmp/stagebadger-test.mp4")),
            &encoder,
            false,
            &AudioFilters::default(),
            &VideoCorrection::default(),
        );
        assert!(args.contains(&"Capture screen 0:".to_string()));
        assert!(args.contains(&"MacBook Pro Camera:".to_string()));
        assert!(args.contains(&":Mic".to_string()));
        assert!(args.contains(&"2:a".to_string()));
        let filter = args
            .iter()
            .position(|arg| arg == "-filter_complex")
            .and_then(|index| args.get(index + 1))
            .unwrap();
        assert!(filter.contains("[program0][pip0]overlay"));
        assert!(filter.contains("x=W-w-24:y=H-h-24"));
    }

    #[test]
    fn test_validate_video_feeds_rejects_duplicate_pip() {
        let camera = make_video_source(0, "MacBook Pro Camera");
        let feeds = VideoFeedSelection {
            primary: camera.clone(),
            pip: Some(camera),
            layout: crate::types::VideoFeedLayout {
                pip_enabled: true,
                pip_position: PipPosition::BottomRight,
                pip_size_percent: 24.0,
            },
        };
        assert!(validate_video_feeds(&feeds).is_err());
    }

    #[test]
    fn test_redact_ffmpeg_args_hides_stream_key() {
        let args = vec!["rtmps://example/live/super-secret-key".to_string()];
        let redacted = redact_ffmpeg_args(&args);
        assert!(!redacted[0].contains("super-secret-key"));
        assert!(redacted[0].contains("[redacted]"));
    }

    #[test]
    fn test_sanitize_filename_keeps_recording_paths_safe() {
        assert_eq!(sanitize_filename("Stage Badger!"), "stage-badger");
        assert_eq!(sanitize_filename("///"), "stagebadger");
    }
}

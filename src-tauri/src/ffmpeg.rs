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

use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Holds the running FFmpeg child process, if any.
///
/// Wrapped in `Arc<Mutex<_>>` and managed as Tauri state so that
/// `start_stream` and `stop_stream` can coordinate safely.
pub struct StreamState {
    pub process: Option<Child>,
}

impl StreamState {
    /// Create a new idle state with no running process.
    pub fn new() -> Self {
        Self { process: None }
    }
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

/// Available audio and video capture devices on this system.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AvDevices {
    pub video: Vec<String>,
    pub audio: Vec<String>,
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
        if let Some(start) = line.find("] [") {
            let after_bracket = &line[start + 3..];
            if let Some(end_idx) = after_bracket.find("] ") {
                let name = &after_bracket[end_idx + 2..];
                if !name.is_empty() {
                    if is_video {
                        video_devices.push(name.to_string());
                    } else {
                        audio_devices.push(name.to_string());
                    }
                }
            }
        }
    }

    AvDevices {
        video: video_devices,
        audio: audio_devices,
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

    let mut base_filter = if blur_background {
        "[0:v]gblur=sigma=10:steps=2[v_blur];[v_blur]".to_string()
    } else {
        "[0:v]".to_string()
    };

    if !overlay_path.is_empty() && std::path::Path::new(overlay_path).exists() {
        let filter = format!(
            "{}[1:v]overlay=W-w-20:20[v1];[v1]{},{}[v_out]",
            base_filter, drawtext_chat, drawtext_asr
        );
        (filter, true)
    } else {
        // We ensure the filter graph takes base_filter directly and passes it along. 
        // Note: For simple drawtext without overlay, it modifies to the implicitly passed stream unless named if not starting complex
        let filter = if blur_background {
            format!("{} {},{}[v_out]", base_filter, drawtext_chat, drawtext_asr)
        } else {
            format!("{},{}[v_out]", drawtext_chat, drawtext_asr)
        };
        (filter, false)
    }
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

/// Start a broadcast session.
///
/// Initializes overlay text files, builds the FFmpeg argument vector,
/// spawns the child process, and kicks off ASR and chat background workers.
pub async fn start(
    app: tauri::AppHandle,
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

    let args = build_ffmpeg_args(&cam, &mic, &overlay_path, &server_url, &key, enable_recording, blur_background);

    let child = Command::new("ffmpeg")
        .args(&args)
        .spawn()
        .map_err(|e| format!("Failed to start FFmpeg: {}", e))?;

    st.process = Some(child);

    // Spawn background workers for overlay content
    crate::chat::spawn_chat_worker();
    crate::asr::spawn_native_asr_worker(app);

    Ok(())
}

/// Stop the active broadcast by killing the FFmpeg child process.
///
/// This is safe to call even if no stream is running (it will return `Ok(())`).
pub async fn stop(state: tauri::State<'_, Arc<Mutex<StreamState>>>) -> Result<(), String> {
    let mut st = state.lock().await;
    if let Some(mut child) = st.process.take() {
        let _ = child.kill().await;
    }
    Ok(())
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
}

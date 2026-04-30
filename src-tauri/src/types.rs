use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SessionPhase {
    Idle,
    Preview,
    Recording,
    Connecting,
    Live,
    Stopping,
    Error,
}

impl Default for SessionPhase {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DestinationKind {
    YoutubeOauth,
    YoutubeRtmps,
    ManualRtmp,
    RecordOnly,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DestinationConfig {
    pub kind: DestinationKind,
    pub label: String,
    #[serde(default)]
    pub manual_destination_id: Option<String>,
    pub rtmp_url: Option<String>,
    pub stream_key: Option<String>,
    pub broadcast_id: Option<String>,
    pub stream_id: Option<String>,
    pub live_chat_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingProfile {
    pub enabled: bool,
    pub directory: Option<String>,
    pub filename_prefix: String,
    pub compact_after_stop: bool,
}

impl Default for RecordingProfile {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: None,
            filename_prefix: "stagebadger".to_string(),
            compact_after_stop: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncoderProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub hevc_compact: bool,
}

impl Default for EncoderProfile {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            video_bitrate_kbps: 6000,
            audio_bitrate_kbps: 160,
            hevc_compact: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OverlayItem {
    pub id: String,
    pub name: String,
    pub source_path: Option<String>,
    pub asset_path: String,
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub opacity: f32,
    pub z_index: i32,
    pub visible: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VideoSourceKind {
    Camera,
    Screen,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VideoSource {
    pub id: String,
    pub label: String,
    pub kind: VideoSourceKind,
    pub avfoundation_name: String,
    pub index: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PipPosition {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoFeedLayout {
    pub pip_enabled: bool,
    pub pip_position: PipPosition,
    pub pip_size_percent: f32,
}

impl Default for VideoFeedLayout {
    fn default() -> Self {
        Self {
            pip_enabled: false,
            pip_position: PipPosition::BottomRight,
            pip_size_percent: 24.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoFeedSelection {
    pub primary: VideoSource,
    pub pip: Option<VideoSource>,
    pub layout: VideoFeedLayout,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub author: String,
    pub message: String,
    pub role: Option<String>,
    pub published_at: Option<String>,
    pub amount_display: Option<String>,
    pub is_super_chat: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptWord {
    pub text: String,
    pub normalized_text: String,
    pub confidence: f32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub source_model: String,
    pub chunk_id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptAlternate {
    pub model_name: String,
    pub confidence: f32,
    pub text: String,
    pub words: Vec<TranscriptWord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: String,
    pub chunk_id: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub confidence: f32,
    pub source_model: String,
    pub text: String,
    pub words: Vec<TranscriptWord>,
    #[serde(default)]
    pub alternates: Vec<TranscriptAlternate>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptFinalization {
    pub is_final: bool,
    pub finalized_at_ms: Option<u64>,
    pub source_media_path: Option<String>,
    pub final_media_path: Option<String>,
    #[serde(default)]
    pub sidecar_paths: Vec<String>,
    pub audio_source: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptDocument {
    pub schema_version: u32,
    pub session_id: String,
    pub source_label: String,
    pub mic_id: Option<String>,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub finalization: TranscriptFinalization,
    #[serde(default)]
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptLiveUpdate {
    pub chunk_id: u64,
    pub text: String,
    pub confidence: f32,
    pub source_model: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptToken {
    pub text: String,
    pub confidence: f32,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub is_final: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegTelemetry {
    pub frame: Option<u64>,
    pub fps: Option<f32>,
    pub bitrate_kbps: Option<f32>,
    pub speed: Option<f32>,
    pub dropped_frames: u64,
    pub errors: u64,
    pub last_line: Option<String>,
    pub exit_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    pub phase: SessionPhase,
    pub destination: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub path: Option<String>,
    pub compacted_path: Option<String>,
    pub duration_ms: u64,
    pub bytes_written: u64,
    pub bitrate_kbps: Option<f32>,
    pub compacted_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct YoutubeStatus {
    pub connected: bool,
    pub message: String,
    pub broadcast_id: Option<String>,
    pub stream_id: Option<String>,
    pub live_chat_id: Option<String>,
    pub ingest_url: Option<String>,
    pub stream_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManualDestination {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub server_url: String,
    pub has_saved_key: bool,
    pub last_used_at: Option<u64>,
    pub default_privacy_note: Option<String>,
    pub confirmed_live_enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManualDestinationSaveRequest {
    pub id: Option<String>,
    pub label: String,
    pub provider: String,
    pub server_url: String,
    pub stream_key: Option<String>,
    pub default_privacy_note: Option<String>,
    pub confirmed_live_enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretWriteRequest {
    pub destination_id: String,
    pub stream_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManualDestinationTestInput {
    pub server_url: String,
    pub stream_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManualDestinationTestRequest {
    pub destination_id: Option<String>,
    pub inline_destination: Option<ManualDestinationTestInput>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DestinationTestResult {
    pub ok: bool,
    pub normalized_server_url: Option<String>,
    pub redacted_url: Option<String>,
    pub message: String,
}

impl DestinationTestResult {
    pub fn failed(message: String) -> Self {
        Self {
            ok: false,
            normalized_server_url: None,
            redacted_url: None,
            message,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OverlayStatus {
    pub overlays: Vec<OverlayItem>,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoEngineStatus {
    pub engine: String,
    pub depth_of_field: bool,
    pub fallback_active: bool,
    pub queue_depth: u32,
    pub dropped_frames: u64,
    pub message: String,
}

impl Default for VideoEngineStatus {
    fn default() -> Self {
        Self {
            engine: "direct-ffmpeg".to_string(),
            depth_of_field: false,
            fallback_active: true,
            queue_depth: 0,
            dropped_frames: 0,
            message: "Direct FFmpeg capture is ready".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub phase: SessionPhase,
    pub destination: Option<DestinationConfig>,
    pub recording_path: Option<String>,
    pub compacted_path: Option<String>,
    pub started_at_ms: Option<u64>,
    pub duration_ms: u64,
    pub bytes_written: u64,
    pub compacted_bytes: Option<u64>,
    pub bitrate_kbps: Option<f32>,
    pub telemetry: FfmpegTelemetry,
    pub video_engine: VideoEngineStatus,
    pub overlays: Vec<OverlayItem>,
    pub error: Option<String>,
}

impl Default for SessionStatus {
    fn default() -> Self {
        Self {
            phase: SessionPhase::Idle,
            destination: None,
            recording_path: None,
            compacted_path: None,
            started_at_ms: None,
            duration_ms: 0,
            bytes_written: 0,
            compacted_bytes: None,
            bitrate_kbps: None,
            telemetry: FfmpegTelemetry::default(),
            video_engine: VideoEngineStatus::default(),
            overlays: Vec::new(),
            error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionRequest {
    pub destination: DestinationConfig,
    pub recording: RecordingProfile,
    pub encoder: EncoderProfile,
    pub camera_id: String,
    #[serde(default)]
    pub video_feeds: Option<VideoFeedSelection>,
    pub mic_id: String,
    pub overlays: Vec<OverlayItem>,
    pub depth_of_field: bool,
    #[serde(default)]
    pub audio_filters: AudioFilters,
    #[serde(default)]
    pub video_correction: VideoCorrection,
}

/// Audio processing filters applied via FFmpeg's audio filter chain.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioFilters {
    /// Enable FFmpeg's `afftdn` noise suppression filter
    pub noise_suppression: bool,
    /// Noise suppression strength (0.0 = light, 1.0 = aggressive). Maps to afftdn nr param.
    pub noise_suppression_level: f32,
    /// Enable FFmpeg's `acompressor` for dynamic range compression
    pub compressor: bool,
    /// Enable FFmpeg's `agate` noise gate to mute below threshold
    pub noise_gate: bool,
    /// Noise gate threshold in dB (e.g. -30.0)
    pub noise_gate_threshold_db: f32,
    /// Volume gain in dB (positive = louder, negative = quieter). Maps to `volume` filter.
    pub gain_db: f32,
}

impl Default for AudioFilters {
    fn default() -> Self {
        Self {
            noise_suppression: false,
            noise_suppression_level: 0.5,
            compressor: false,
            noise_gate: false,
            noise_gate_threshold_db: -30.0,
            gain_db: 0.0,
        }
    }
}

/// Real-time video color correction applied via FFmpeg's `eq` filter.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoCorrection {
    pub enabled: bool,
    /// Brightness adjustment (-1.0 to 1.0, default 0.0)
    pub brightness: f32,
    /// Contrast multiplier (0.0 to 3.0, default 1.0)
    pub contrast: f32,
    /// Saturation multiplier (0.0 to 3.0, default 1.0)
    pub saturation: f32,
    /// Gamma (0.1 to 10.0, default 1.0)
    pub gamma: f32,
}

impl Default for VideoCorrection {
    fn default() -> Self {
        Self {
            enabled: false,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
        }
    }
}

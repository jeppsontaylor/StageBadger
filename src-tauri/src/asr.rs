//! # Dual-Model ASR Pipeline
//!
//! This module implements the dual-ASR architecture where two Whisper-class models
//! race to transcribe the same audio chunk. The aggregator selects the result with
//! the highest confidence score and writes it to the overlay file.
//!
//! ## Current Status: Mock Implementation
//!
//! The current implementation uses simulated phrase rotation with realistic timing
//! to validate the concurrency architecture. To integrate real inference:
//!
//! 1. Replace the phrase lookup with `whisper-rs` or `candle-whisper` inference
//! 2. Replace the `AtomicUsize` ticker with a `cpal` audio capture buffer
//! 3. The `mpsc` channel and aggregator logic remain unchanged
//!
//! ## File Protocol
//!
//! The aggregator writes the winning transcription to `/tmp/stagebadger_asr.txt`.
//! FFmpeg reads this file every frame via `drawtext reload=1`.

use std::fs::{self, File};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use reqwest::Client;
use futures_util::StreamExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A granular token from the Whisper inference engine.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AsrToken {
    pub text: String,
    pub prob: f32, // 0.0 to 1.0
}

/// A transcription result from one of the ASR models.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AsrResult {
    /// The transcribed text
    pub text: String,
    /// Granular token probabilities
    pub tokens: Vec<AsrToken>,
    /// Confidence score in [0.0, 1.0]
    pub confidence: f64,
    /// Which model produced this result
    pub model_name: String,
}

impl AsrResult {
    /// Create a new ASR result.
    pub fn new(text: impl Into<String>, confidence: f64, model_name: impl Into<String>, tokens: Vec<AsrToken>) -> Self {
        Self {
            text: text.into(),
            tokens,
            confidence,
            model_name: model_name.into(),
        }
    }
}

/// Format an ASR result for overlay display.
///
/// Output format: `"transcribed text [0.92 model-name]"`
pub fn format_asr_display(result: &AsrResult) -> String {
    format!("{} [{:.2} {}]", result.text, result.confidence, result.model_name)
}

/// Select the best result from a batch of ASR results.
///
/// Returns the result with the highest confidence score.
/// Returns `None` if the input is empty.
pub fn select_best_result(results: &[AsrResult]) -> Option<&AsrResult> {
    results
        .iter()
        .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal))
}


/// Spawn native hardware capture into the transcription engine.
pub fn spawn_native_asr_worker(app: tauri::AppHandle) {
    tokio::task::spawn_blocking(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use tauri::Emitter;
        
        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(dev) => dev,
            None => {
                println!("WARNING: No microphone found for CPAL hardware capture!");
                return;
            }
        };
        
        let config = device.default_input_config().unwrap();
        let channels = config.channels() as usize;
        let sample_rate = config.sample_rate() as u32;
        
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let buffer_clone = std::sync::Arc::clone(&buffer);
        
        let err_fn = move |err| println!("CPAL error: {}", err);
        
        let model_path = "/Volumes/MOE/models/ggml-tiny.en.bin";
        let model_url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin";

        // Bootstrap Check: Auto-download the GGML Weights if they don't natively exist!
        if !std::path::Path::new(model_path).exists() {
            println!("Bootstrapping: GGML missing! Downloading {} to {}...", model_url, model_path);
            let result = tokio::runtime::Handle::current().block_on(async {
                download_ggml_model(model_url, std::path::Path::new(model_path)).await
            });
            if let Err(e) = result {
                println!("CRITICAL: ASR Tensor Download Failed! {}", e);
                return;
            }
            println!("Success: ASR ggml-tiny weights securely pulled!");
        }

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        let mut b = buffer_clone.lock().unwrap();
                        for chunk in data.chunks(channels) {
                            b.push(chunk[0]); // Downmix Mono
                        }
                    },
                    err_fn,
                    None,
                )
            },
            _ => panic!("Hardware Microphone format unsupported! Required: F32 (Apple Metal Native)."),
        }.unwrap();
        
        stream.play().unwrap();
        
        loop {
            std::thread::sleep(std::time::Duration::from_millis(3500));
            
            let mut b = buffer.lock().unwrap();
            let mut pcm_data = b.clone();
            b.clear();
            drop(b);
            
            if pcm_data.len() > 16000 {
                if sample_rate == 48000 {
                    pcm_data = pcm_data.into_iter().step_by(3).collect();
                } else if sample_rate == 44100 {
                    pcm_data = pcm_data.into_iter().step_by(3).collect();
                }
                
                match run_whisper_inference(model_path, &pcm_data) {
                    Ok(result) => {
                        let _ = app.emit("asr_stream", result);
                    },
                    Err(e) => {
                        println!("ASR Inference Exception Dropped: {}", e);
                    }
                }
            }
        }
    });
}

/// Dynamically downloads a Whisper GGML model from the given URL to the destination path.
pub async fn download_ggml_model(url: &str, dest: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    
    let client = Client::new();
    let res = client.get(url).send().await.map_err(|e| e.to_string())?;
    
    if !res.status().is_success() {
        return Err(format!("Download failed with status: {}", res.status()));
    }
    
    let mut file = File::create(dest).map_err(|e| e.to_string())?;
    let mut stream = res.bytes_stream();
    
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    
    Ok(())
}

/// Run real Whisper C++ inference natively over a 16kHz PCM f32 audio array.
pub fn run_whisper_inference(model_path: &str, pcm_data: &[f32]) -> Result<AsrResult, String> {
    let ctx_params = WhisperContextParameters::default();
    let ctx = WhisperContext::new_with_params(model_path, ctx_params)
        .map_err(|e| format!("Failed to load whisper model: {}", e))?;
    
    let mut state = ctx.create_state().map_err(|e| format!("Failed to create state: {}", e))?;
    
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, pcm_data).map_err(|e| format!("Inference failed: {}", e))?;
    
    let num_segments = state.full_n_segments();
    let mut transcription = String::new();
    let mut tokens = Vec::new();
    
    for i in 0..num_segments {
        if let Some(segment) = state.get_segment(i) {
            if let Ok(text) = segment.to_str() {
                transcription.push_str(text);
            }
            // Extract token-level confidences
            for token_i in 0..segment.n_tokens() {
                if let Some(token) = segment.get_token(token_i) {
                    if let Ok(token_text) = token.to_str_lossy() {
                        let text = token_text.to_string();
                        if !text.trim().is_empty() {
                            tokens.push(AsrToken {
                                text,
                                prob: token.token_probability()
                            });
                        }
                    }
                }
            }
        }
    }
    
    // Average confidence across all valid tokens
    let confidence = if tokens.is_empty() {
        1.0
    } else {
        (tokens.iter().map(|t| t.prob).sum::<f32>() / tokens.len() as f32) as f64
    };

    Ok(AsrResult::new(transcription.trim(), confidence, "whisper-rs-native", tokens))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asr_result_new() {
        let result = AsrResult::new("hello world", 0.95, "whisper-tiny", vec![]);
        assert_eq!(result.text, "hello world");
        assert_eq!(result.confidence, 0.95);
        assert_eq!(result.model_name, "whisper-tiny");
    }

    #[test]
    fn test_format_asr_display() {
        let result = AsrResult::new("testing one two three", 0.87, "distil-whisper", vec![]);
        let display = format_asr_display(&result);
        assert_eq!(display, "testing one two three [0.87 distil-whisper]");
    }

    #[test]
    fn test_format_asr_display_high_confidence() {
        let result = AsrResult::new("perfect clarity", 1.0, "whisper-large");
        let display = format_asr_display(&result);
        assert_eq!(display, "perfect clarity [1.00 whisper-large]");
    }

    #[test]
    fn test_select_best_result_picks_highest_confidence() {
        let results = vec![
            AsrResult::new("low confidence", 0.5, "model-a", vec![]),
            AsrResult::new("high confidence", 0.95, "model-b", vec![]),
            AsrResult::new("medium confidence", 0.7, "model-c", vec![]),
        ];

        let best = select_best_result(&results).unwrap();
        assert_eq!(best.text, "high confidence");
        assert_eq!(best.confidence, 0.95);
        assert_eq!(best.model_name, "model-b");
    }

    #[test]
    fn test_select_best_result_empty() {
        let results: Vec<AsrResult> = vec![];
        assert!(select_best_result(&results).is_none());
    }

    #[test]
    fn test_select_best_result_single() {
        let results = vec![AsrResult::new("only one", 0.88, "model-a", vec![])];
        let best = select_best_result(&results).unwrap();
        assert_eq!(best.text, "only one");
    }

    #[test]
    fn test_select_best_result_tie() {
        let results = vec![
            AsrResult::new("first", 0.90, "model-a"),
            AsrResult::new("second", 0.90, "model-b"),
        ];
        // Either is acceptable — just verify it returns something
        let best = select_best_result(&results).unwrap();
        assert_eq!(best.confidence, 0.90);
    }

    #[test]
    fn test_mock_phrases_not_empty() {
        assert!(!MOCK_PHRASES.is_empty());
        for phrase in MOCK_PHRASES {
            assert!(!phrase.is_empty());
        }
    }

    #[test]
    fn test_asr_result_clone() {
        let original = AsrResult::new("test", 0.9, "model", vec![]);
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }
}

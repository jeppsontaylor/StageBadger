//! # Dual-Model ASR Pipeline
//!
//! Real-time speech-to-text via whisper-rs with Metal GPU acceleration on Apple Silicon.
//!
//! ## Architecture
//!
//! ```text
//!  [CPAL Mic 48kHz] → [Shared Buffer] → [Coordinator (2s snap)]
//!                                              ↓
//!                                    ┌─────────┴──────────┐
//!                                    ↓                    ↓
//!                            [Worker A: tiny]     [Worker B: base]
//!                                    ↓                    ↓
//!                                    └─────────┬──────────┘
//!                                              ↓
//!                                  [Best-of-2 Merge + Emit]
//! ```
//!
//! - **Coordinator**: Every 2s, snapshots the audio buffer, sends the SAME chunk
//!   to both workers via channels, collects results with a deadline, picks the best.
//! - **Workers**: Each loads its own model once, receives chunks, returns results.
//! - **Merge logic**: If both respond, pick the higher-confidence result. If only
//!   one responds within the deadline, use that one.

use std::fs::{self, File};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::Client;
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
pub fn format_asr_display(result: &AsrResult) -> String {
    format!("{} [{:.2} {}]", result.text, result.confidence, result.model_name)
}

/// Select the best result from a batch of ASR results.
///
/// Uses a scoring formula that combines:
/// - Token-level average confidence (60% weight)
/// - Text length bonus — longer coherent text is preferred (20% weight)
/// - Token count — more granular tokens means better decomposition (20% weight)
pub fn select_best_result(results: &[AsrResult]) -> Option<&AsrResult> {
    if results.is_empty() {
        return None;
    }
    if results.len() == 1 {
        return Some(&results[0]);
    }

    results.iter().max_by(|a, b| {
        let score_a = compute_merge_score(a);
        let score_b = compute_merge_score(b);
        score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Composite scoring formula for ASR result quality.
///
/// Score = 0.60 * avg_confidence + 0.20 * length_factor + 0.20 * token_density
///
/// - `avg_confidence`: average token probability [0..1]
/// - `length_factor`: clamped text length / 100 (longer = better, up to a point)
/// - `token_density`: number of meaningful tokens / 20 (more = better decomp)
fn compute_merge_score(result: &AsrResult) -> f64 {
    let avg_conf = result.confidence;
    let length_factor = (result.text.trim().len() as f64 / 100.0).min(1.0);
    let token_density = (result.tokens.len() as f64 / 20.0).min(1.0);
    0.60 * avg_conf + 0.20 * length_factor + 0.20 * token_density
}

/// Resample audio from an arbitrary source rate to 16kHz using linear interpolation.
pub fn resample_to_16k(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16000 {
        return samples.to_vec();
    }

    let ratio = source_rate as f64 / 16000.0;
    let output_len = (samples.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let src_idx = i as f64 * ratio;
        let idx_floor = src_idx.floor() as usize;
        let frac = (src_idx - idx_floor as f64) as f32;

        if idx_floor + 1 < samples.len() {
            let sample = samples[idx_floor] * (1.0 - frac) + samples[idx_floor + 1] * frac;
            output.push(sample);
        } else if idx_floor < samples.len() {
            output.push(samples[idx_floor]);
        }
    }

    output
}

/// Check if an inference result should be filtered out (noise/hallucinations).
fn should_filter_result(text: &str) -> bool {
    text.is_empty()
        || text.contains("[BLANK_AUDIO]")
        || text.contains("[BLANK")
        || text.starts_with('[')
        || text == "."
        || text == "..."
        || text.contains("(")
        || text.contains("Thank you")
        || text.contains("Thanks for watching")
        || text.contains("Bye.")
}

/// Filter special/noise tokens from the token list.
fn filter_tokens(tokens: &mut Vec<AsrToken>) {
    tokens.retain(|t| {
        let s = t.text.trim();
        !s.is_empty() && !s.starts_with('[') && !s.starts_with('(') && !s.contains("BLANK")
    });
}

/// Model configuration for a worker thread.
struct ModelConfig {
    name: &'static str,
    path: &'static str,
    download_url: &'static str,
}

const MODEL_TINY: ModelConfig = ModelConfig {
    name: "tiny",
    path: "/Volumes/MOE/models/ggml-tiny.en.bin",
    download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
};

const MODEL_BASE: ModelConfig = ModelConfig {
    name: "base",
    path: "/Volumes/MOE/models/ggml-base.en.bin",
    download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
};

/// Maximum time to wait for the slower model before emitting the faster one.
const MERGE_DEADLINE_MS: u64 = 800;

/// Spawn the dual-model ASR pipeline with coordinated merge.
///
/// Architecture:
/// 1. **Capture thread**: CPAL fills a shared audio buffer
/// 2. **Coordinator thread**: Every 2s, snapshots audio, sends same chunk to both workers,
///    waits for results with deadline, picks best, emits once
/// 3. **Worker A** (tiny): fast first-pass
/// 4. **Worker B** (base): accuracy pass
pub fn spawn_native_asr_worker(app: tauri::AppHandle, shutdown: Arc<AtomicBool>) {
    // Shared audio buffer — capture thread writes, coordinator reads+clears
    let buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::new()));

    // --- Audio Capture Thread ---
    let buf_capture = Arc::clone(&buffer);
    let shutdown_capture = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(dev) => dev,
            None => {
                println!("ASR: WARNING — No microphone found!");
                return;
            }
        };

        let config = device.default_input_config().unwrap();
        let channels = config.channels() as usize;
        let sample_rate = config.sample_rate() as u32;

        println!(
            "ASR: Mic — {}Hz, {} ch, {:?}",
            sample_rate,
            channels,
            config.sample_format()
        );

        let buf_cb = Arc::clone(&buf_capture);
        let err_fn = move |err| println!("ASR: CPAL error: {}", err);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        let mut b = buf_cb.lock().unwrap();
                        for chunk in data.chunks(channels) {
                            b.push(chunk[0]); // Mono downmix
                        }
                    },
                    err_fn,
                    None,
                )
            }
            _ => panic!("ASR: Unsupported mic format (need F32)"),
        }
        .unwrap();

        stream.play().unwrap();
        println!("ASR: CPAL capture LIVE");

        while !shutdown_capture.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(stream);
        println!("ASR: Capture thread stopped.");
    });

    // --- Channels for coordinator ↔ worker communication ---
    // Coordinator sends audio chunks to workers
    let (tx_chunk_a, rx_chunk_a) = std::sync::mpsc::channel::<Vec<f32>>();
    let (tx_chunk_b, rx_chunk_b) = std::sync::mpsc::channel::<Vec<f32>>();
    // Workers send results back to coordinator
    let (tx_result, rx_result) = std::sync::mpsc::channel::<(String, AsrResult)>();

    // --- Worker A (tiny, fast) ---
    let tx_res_a = tx_result.clone();
    let shutdown_a = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        run_worker_loop(&MODEL_TINY, rx_chunk_a, tx_res_a, shutdown_a);
    });

    // --- Worker B (base, accurate) ---
    let tx_res_b = tx_result;
    let shutdown_b = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        run_worker_loop(&MODEL_BASE, rx_chunk_b, tx_res_b, shutdown_b);
    });

    // --- Coordinator Thread ---
    let shutdown_coord = Arc::clone(&shutdown);
    std::thread::spawn(move || {
        use tauri::Emitter;

        println!(
            "ASR: Coordinator LIVE — 2s chunks, {}ms merge deadline",
            MERGE_DEADLINE_MS
        );
        let mut chunk_id: u64 = 0;
        let source_rate: u32 = 48000;

        loop {
            if shutdown_coord.load(Ordering::Relaxed) {
                println!("ASR: Coordinator shutdown.");
                break;
            }

            // Wait 2 seconds to accumulate audio
            std::thread::sleep(Duration::from_millis(2000));

            if shutdown_coord.load(Ordering::Relaxed) {
                break;
            }

            // Snapshot and clear the shared buffer
            let raw_pcm = {
                let mut b = buffer.lock().unwrap();
                let data = b.clone();
                b.clear();
                data
            };

            // Need at least ~0.5s of audio at source rate
            if raw_pcm.len() < (source_rate as usize / 2) {
                continue;
            }

            chunk_id += 1;
            let pcm_16k = resample_to_16k(&raw_pcm, source_rate);

            // Send the SAME chunk to both workers
            let _ = tx_chunk_a.send(pcm_16k.clone());
            let _ = tx_chunk_b.send(pcm_16k);

            // Collect results with deadline
            let t0 = Instant::now();
            let deadline = Duration::from_millis(MERGE_DEADLINE_MS);
            let mut results: Vec<(String, AsrResult)> = Vec::with_capacity(2);

            // Wait for up to 2 results within the deadline
            while results.len() < 2 {
                let remaining = deadline.saturating_sub(t0.elapsed());
                if remaining.is_zero() {
                    break;
                }
                match rx_result.recv_timeout(remaining) {
                    Ok(result) => results.push(result),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }

            if results.is_empty() {
                continue;
            }

            // Pick the best using composite scoring
            let best_idx = if results.len() == 1 {
                0
            } else {
                let score_0 = compute_merge_score(&results[0].1);
                let score_1 = compute_merge_score(&results[1].1);
                if score_1 > score_0 {
                    1
                } else {
                    0
                }
            };

            // Gather names before consuming
            let model_names: Vec<String> = results.iter().map(|(n, _)| n.clone()).collect();

            // Consume the winner
            let (winner_model, best_result) = results.swap_remove(best_idx);

            // Write to FFmpeg overlay file
            let _ = fs::write("/tmp/stagebadger_asr.txt", &best_result.text);

            // Emit single best result to frontend
            let _ = app.emit("asr_stream", &best_result);
            let _ = app.emit("asr_partial", &best_result);
            let _ = app.emit("asr_final", &best_result);

            let elapsed = t0.elapsed();
            println!(
                "ASR #{}: [WINNER: {}] conf={:.2} score={:.3} \"{}\" ({}ms, models: {:?})",
                chunk_id,
                winner_model,
                best_result.confidence,
                compute_merge_score(&best_result),
                best_result.text.trim(),
                elapsed.as_millis(),
                model_names,
            );
        }
    });
}

/// Worker loop: receives audio chunks via channel, runs inference, sends results back.
fn run_worker_loop(
    config: &'static ModelConfig,
    rx: std::sync::mpsc::Receiver<Vec<f32>>,
    tx: std::sync::mpsc::Sender<(String, AsrResult)>,
    shutdown: Arc<AtomicBool>,
) {
    // Bootstrap: download model if missing
    if !std::path::Path::new(config.path).exists() {
        println!("ASR [{}]: Model missing, downloading...", config.name);
        match download_ggml_model_sync(config.download_url, std::path::Path::new(config.path)) {
            Ok(_) => println!("ASR [{}]: Model downloaded OK", config.name),
            Err(e) => {
                println!("ASR [{}]: CRITICAL download failed: {}", config.name, e);
                return;
            }
        }
    }

    // Load model ONCE
    let ctx_params = WhisperContextParameters::default();
    let ctx = match WhisperContext::new_with_params(config.path, ctx_params) {
        Ok(c) => {
            println!("ASR [{}]: Model loaded, Metal GPU active", config.name);
            c
        }
        Err(e) => {
            println!("ASR [{}]: CRITICAL load failed: {}", config.name, e);
            return;
        }
    };

    println!("ASR [{}]: Worker LIVE, waiting for chunks...", config.name);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            println!("ASR [{}]: Shutdown.", config.name);
            break;
        }

        // Block until coordinator sends a chunk (or timeout for shutdown check)
        let pcm_16k = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(data) => data,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let t0 = Instant::now();

        match run_whisper_inference_with_ctx(&ctx, &pcm_16k) {
            Ok(mut result) => {
                let text = result.text.trim().to_string();
                let elapsed_ms = t0.elapsed().as_millis();

                if should_filter_result(&text) {
                    println!("ASR [{}]: Filtered noise in {}ms", config.name, elapsed_ms);
                    continue;
                }

                filter_tokens(&mut result.tokens);
                if result.tokens.is_empty() {
                    continue;
                }

                result.model_name = config.name.to_string();

                println!(
                    "ASR [{}]: \"{}\" conf={:.2} tokens={} ({}ms)",
                    config.name,
                    text,
                    result.confidence,
                    result.tokens.len(),
                    elapsed_ms
                );

                // Send result back to coordinator for merge
                let _ = tx.send((config.name.to_string(), result));
            }
            Err(e) => {
                println!("ASR [{}]: Inference error: {}", config.name, e);
            }
        }
    }

    println!("ASR [{}]: Worker terminated.", config.name);
}

/// Run Whisper inference using a pre-loaded context (zero model reload overhead).
pub fn run_whisper_inference_with_ctx(ctx: &WhisperContext, pcm_data: &[f32]) -> Result<AsrResult, String> {
    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create state: {}", e))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, pcm_data)
        .map_err(|e| format!("Inference failed: {}", e))?;

    let num_segments = state.full_n_segments();
    let mut transcription = String::new();
    let mut tokens = Vec::new();

    for i in 0..num_segments {
        if let Some(segment) = state.get_segment(i) {
            if let Ok(text) = segment.to_str() {
                transcription.push_str(text);
            }
            for token_i in 0..segment.n_tokens() {
                if let Some(token) = segment.get_token(token_i) {
                    if let Ok(token_text) = token.to_str_lossy() {
                        let text = token_text.to_string();
                        if !text.trim().is_empty() {
                            tokens.push(AsrToken {
                                text,
                                prob: token.token_probability(),
                            });
                        }
                    }
                }
            }
        }
    }

    let confidence = if tokens.is_empty() {
        1.0
    } else {
        (tokens.iter().map(|t| t.prob).sum::<f32>() / tokens.len() as f32) as f64
    };

    Ok(AsrResult::new(transcription.trim(), confidence, "whisper-rs", tokens))
}

/// Dynamically downloads a Whisper GGML model (async).
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

/// Synchronous version of model download for `std::thread` contexts.
pub fn download_ggml_model_sync(url: &str, dest: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let res = reqwest::blocking::get(url).map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("Download failed with status: {}", res.status()));
    }

    let bytes = res.bytes().map_err(|e| e.to_string())?;
    let mut file = File::create(dest).map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;

    Ok(())
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
    }

    #[test]
    fn test_format_asr_display() {
        let result = AsrResult::new("testing one two three", 0.87, "distil-whisper", vec![]);
        let display = format_asr_display(&result);
        assert_eq!(display, "testing one two three [0.87 distil-whisper]");
    }

    #[test]
    fn test_format_asr_display_high_confidence() {
        let result = AsrResult::new("perfect clarity", 1.0, "whisper-large", vec![]);
        let display = format_asr_display(&result);
        assert_eq!(display, "perfect clarity [1.00 whisper-large]");
    }

    #[test]
    fn test_select_best_result_picks_highest_score() {
        let r_low = AsrResult::new(
            "short",
            0.5,
            "model-a",
            vec![AsrToken {
                text: "short".into(),
                prob: 0.5,
            }],
        );
        let r_high = AsrResult::new(
            "this is a much better transcription",
            0.95,
            "model-b",
            vec![
                AsrToken {
                    text: " this".into(),
                    prob: 0.96,
                },
                AsrToken {
                    text: " is".into(),
                    prob: 0.98,
                },
                AsrToken {
                    text: " a".into(),
                    prob: 0.95,
                },
                AsrToken {
                    text: " much".into(),
                    prob: 0.92,
                },
                AsrToken {
                    text: " better".into(),
                    prob: 0.94,
                },
                AsrToken {
                    text: " transcription".into(),
                    prob: 0.93,
                },
            ],
        );

        let results = vec![r_low, r_high];
        let best = select_best_result(&results).unwrap();
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
            AsrResult::new("first", 0.90, "model-a", vec![]),
            AsrResult::new("second", 0.90, "model-b", vec![]),
        ];
        let best = select_best_result(&results).unwrap();
        assert_eq!(best.confidence, 0.90);
    }

    #[test]
    fn test_asr_result_clone() {
        let original = AsrResult::new("test", 0.9, "model", vec![]);
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn test_resample_passthrough_16k() {
        let input = vec![1.0, 2.0, 3.0, 4.0];
        let output = resample_to_16k(&input, 16000);
        assert_eq!(output, input);
    }

    #[test]
    fn test_resample_48k_to_16k() {
        let input: Vec<f32> = (0..9).map(|i| i as f32).collect();
        let output = resample_to_16k(&input, 48000);
        assert_eq!(output.len(), 3);
        assert!((output[0] - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_resample_44100_to_16k() {
        let input: Vec<f32> = (0..100).map(|i| (i as f32).sin()).collect();
        let output = resample_to_16k(&input, 44100);
        let expected_len = (100.0 / (44100.0 / 16000.0)) as usize;
        assert!((output.len() as i32 - expected_len as i32).abs() <= 1);
    }

    #[test]
    fn test_should_filter_result_blank_audio() {
        assert!(should_filter_result("[BLANK_AUDIO]"));
        assert!(should_filter_result("[BLANK"));
        assert!(should_filter_result(""));
        assert!(should_filter_result("."));
        assert!(should_filter_result("..."));
        assert!(should_filter_result("(water splashing)"));
        assert!(!should_filter_result("Hello world"));
    }

    #[test]
    fn test_filter_tokens_removes_specials() {
        let mut tokens = vec![
            AsrToken {
                text: " Hello".into(),
                prob: 0.95,
            },
            AsrToken {
                text: "[BLANK".into(),
                prob: 0.80,
            },
            AsrToken {
                text: " world".into(),
                prob: 0.90,
            },
            AsrToken {
                text: "(noise)".into(),
                prob: 0.30,
            },
        ];
        filter_tokens(&mut tokens);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, " Hello");
        assert_eq!(tokens[1].text, " world");
    }

    #[test]
    fn test_compute_merge_score_prefers_quality() {
        let low_quality = AsrResult::new(
            "hi",
            0.4,
            "tiny",
            vec![AsrToken {
                text: "hi".into(),
                prob: 0.4,
            }],
        );
        let high_quality = AsrResult::new(
            "Hello, how are you doing today?",
            0.92,
            "base",
            vec![
                AsrToken {
                    text: " Hello".into(),
                    prob: 0.95,
                },
                AsrToken {
                    text: ",".into(),
                    prob: 0.88,
                },
                AsrToken {
                    text: " how".into(),
                    prob: 0.94,
                },
                AsrToken {
                    text: " are".into(),
                    prob: 0.96,
                },
                AsrToken {
                    text: " you".into(),
                    prob: 0.90,
                },
                AsrToken {
                    text: " doing".into(),
                    prob: 0.89,
                },
                AsrToken {
                    text: " today".into(),
                    prob: 0.91,
                },
            ],
        );

        let score_low = compute_merge_score(&low_quality);
        let score_high = compute_merge_score(&high_quality);
        assert!(
            score_high > score_low,
            "High quality score {} should beat low quality {}",
            score_high,
            score_low
        );
    }
}

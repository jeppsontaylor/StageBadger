use std::cmp::{max, min};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use hound::WavReader;
use reqwest::Client;
use tauri::Emitter;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::asr::{download_ggml_model_sync, resample_to_16k};
use crate::types::{
    TranscriptAlternate, TranscriptDocument, TranscriptFinalization, TranscriptLiveUpdate, TranscriptSegment,
    TranscriptWord,
};

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

const SCHEMA_VERSION: u32 = 1;
const LIVE_WINDOW_MS: u64 = 2_000;
const LIVE_MERGE_DEADLINE_MS: u64 = 800;
const FINAL_WINDOW_MS: u64 = 8_000;
const FINAL_WINDOW_HOP_MS: u64 = 6_000;
const MIN_AUDIO_SAMPLES: usize = 8_000;
const CONTEXT_TAIL_WORDS: usize = 12;
const LIVE_CAPTION_PATH: &str = "/tmp/stagebadger_asr.txt";

#[derive(Clone, Copy)]
struct ModelConfig {
    name: &'static str,
    path: &'static str,
    download_url: &'static str,
}

#[derive(Debug, Clone)]
struct ModelSpan {
    model_name: String,
    confidence: f32,
    text: String,
    words: Vec<TranscriptWord>,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Debug, Clone)]
struct LiveSessionRuntime {
    shutdown: Arc<AtomicBool>,
    transcript: Arc<Mutex<LiveTranscriptState>>,
}

#[derive(Debug, Clone)]
struct LiveTranscriptState {
    document: TranscriptDocument,
    committed_words: Vec<TranscriptWord>,
}

#[derive(Debug, Default)]
pub struct AsrRuntime {
    active_session: Option<LiveSessionRuntime>,
}

impl AsrRuntime {
    pub fn new() -> Self {
        Self { active_session: None }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sanitize_text(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn normalize_token_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_noise_text(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty()
        || trimmed.starts_with('[')
        || trimmed.starts_with('(')
        || trimmed.contains("BLANK")
        || trimmed == "."
        || trimmed == "..."
        || trimmed.contains("Thanks for watching")
        || trimmed.contains("Thank you")
        || trimmed.contains("Bye.")
}

fn is_punctuation_only(value: &str) -> bool {
    value.chars().all(|c| !c.is_ascii_alphanumeric())
}

fn centiseconds_to_ms(value: i64) -> u64 {
    if value <= 0 {
        0
    } else {
        value as u64 * 10
    }
}

fn build_prompt_tail(words: &[TranscriptWord]) -> String {
    let mut tail = words
        .iter()
        .rev()
        .take(CONTEXT_TAIL_WORDS)
        .map(|word| word.text.trim().to_string())
        .collect::<Vec<_>>();
    tail.reverse();
    tail.into_iter()
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn trim_overlap_prefix(candidate: &[TranscriptWord], committed_tail: &[TranscriptWord]) -> Vec<TranscriptWord> {
    if candidate.is_empty() {
        return Vec::new();
    }

    let max_overlap = min(candidate.len(), committed_tail.len()).min(CONTEXT_TAIL_WORDS);
    let mut best = 0usize;

    for overlap in 1..=max_overlap {
        let tail_slice = &committed_tail[committed_tail.len() - overlap..];
        let prefix_slice = &candidate[..overlap];
        if tail_slice
            .iter()
            .zip(prefix_slice.iter())
            .all(|(tail, current)| normalize_token_text(&tail.text) == normalize_token_text(&current.text))
        {
            best = overlap;
        }
    }

    candidate[best..].to_vec()
}

fn tail_words(words: &[TranscriptWord], keep: usize) -> Vec<TranscriptWord> {
    if words.len() <= keep {
        words.to_vec()
    } else {
        words[words.len() - keep..].to_vec()
    }
}

fn words_to_text(words: &[TranscriptWord]) -> String {
    let mut text = String::new();
    for word in words {
        let part = word.text.trim();
        if part.is_empty() {
            continue;
        }
        if text.is_empty() {
            text.push_str(part);
            continue;
        }
        if is_punctuation_only(part) {
            text.push_str(part);
        } else {
            if !text.ends_with(' ') {
                text.push(' ');
            }
            text.push_str(part);
        }
    }
    text.trim().to_string()
}

fn format_timestamp_srt(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{:02}:{:02}:{:02},{:03}", hours, minutes, seconds, millis)
}

fn format_timestamp_vtt(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
}

fn build_context_params(model_name: &str) -> WhisperContextParameters<'_> {
    let mut params = WhisperContextParameters::default();
    params.dtw_parameters.mode = whisper_rs::DtwMode::ModelPreset {
        model_preset: match model_name {
            "tiny" => whisper_rs::DtwModelPreset::TinyEn,
            _ => whisper_rs::DtwModelPreset::BaseEn,
        },
    };
    params
}

fn ensure_model(config: &ModelConfig) -> Result<(), String> {
    if Path::new(config.path).exists() {
        return Ok(());
    }

    download_ggml_model_sync(config.download_url, Path::new(config.path))
}

fn load_context(config: &ModelConfig) -> Result<WhisperContext, String> {
    ensure_model(config)?;
    let params = build_context_params(config.name);
    WhisperContext::new_with_params(config.path, params)
        .map_err(|e| format!("Failed to load {} model: {}", config.name, e))
}

fn build_hypothesis(
    ctx: &WhisperContext,
    config: &ModelConfig,
    pcm_data: &[f32],
    chunk_id: u64,
    chunk_start_ms: u64,
    prompt: &str,
) -> Result<ModelSpan, String> {
    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create state: {}", e))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_token_timestamps(true);
    params.set_split_on_word(true);
    params.set_translate(false);
    params.set_n_threads(1);
    if !prompt.trim().is_empty() {
        params.set_initial_prompt(prompt);
    }

    state
        .full(params, pcm_data)
        .map_err(|e| format!("Inference failed for {}: {}", config.name, e))?;

    let mut words = Vec::new();
    let mut raw_text = String::new();

    for segment in state.as_iter() {
        let segment_text = segment
            .to_str_lossy()
            .map_err(|e| format!("Segment text error: {}", e))?;
        let segment_text = segment_text.trim();
        if !segment_text.is_empty() {
            if !raw_text.is_empty() {
                raw_text.push(' ');
            }
            raw_text.push_str(segment_text);
        }

        for token_index in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(token_index) else {
                continue;
            };
            let text = token.to_str_lossy().map_err(|e| format!("Token text error: {}", e))?;
            let trimmed = text.trim();
            if trimmed.is_empty() || is_noise_text(trimmed) {
                continue;
            }

            let token_data = token.token_data();
            let start_ms = if token_data.t0 >= 0 {
                chunk_start_ms + centiseconds_to_ms(token_data.t0)
            } else {
                chunk_start_ms + centiseconds_to_ms(segment.start_timestamp())
            };
            let end_ms = if token_data.t1 >= 0 {
                chunk_start_ms + centiseconds_to_ms(token_data.t1)
            } else if token_data.t0 >= 0 {
                chunk_start_ms + centiseconds_to_ms(token_data.t0 + 1)
            } else {
                chunk_start_ms + centiseconds_to_ms(segment.end_timestamp())
            };

            words.push(TranscriptWord {
                text: trimmed.to_string(),
                normalized_text: normalize_token_text(trimmed),
                confidence: token.token_probability().max(token_data.p),
                start_ms,
                end_ms: max(end_ms, start_ms),
                source_model: config.name.to_string(),
                chunk_id,
            });
        }
    }

    let confidence = if words.is_empty() {
        0.0
    } else {
        words.iter().map(|word| word.confidence).sum::<f32>() / words.len() as f32
    };
    let start_ms = words.first().map(|word| word.start_ms).unwrap_or(chunk_start_ms);
    let end_ms = words.last().map(|word| word.end_ms).unwrap_or(chunk_start_ms);

    Ok(ModelSpan {
        model_name: config.name.to_string(),
        confidence,
        text: if raw_text.is_empty() {
            words_to_text(&words)
        } else {
            raw_text
        },
        words,
        start_ms,
        end_ms,
    })
}

fn word_lcs_ratio(left: &[TranscriptWord], right: &[TranscriptWord]) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let mut table = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for i in 0..left.len() {
        for j in 0..right.len() {
            if left[i].normalized_text == right[j].normalized_text {
                table[i + 1][j + 1] = table[i][j] + 1;
            } else {
                table[i + 1][j + 1] = max(table[i][j + 1], table[i + 1][j]);
            }
        }
    }

    table[left.len()][right.len()] as f32 / max(left.len(), right.len()) as f32
}

fn span_overlap_ratio(left: &ModelSpan, right: &ModelSpan) -> f32 {
    let start = max(left.start_ms, right.start_ms);
    let end = min(left.end_ms, right.end_ms);
    if end <= start {
        return 0.0;
    }

    let overlap = (end - start) as f32;
    let span = max(
        left.end_ms.saturating_sub(left.start_ms),
        right.end_ms.saturating_sub(right.start_ms),
    );
    if span == 0 {
        0.0
    } else {
        overlap / span as f32
    }
}

fn spans_agree(left: &ModelSpan, right: &ModelSpan) -> bool {
    if left.words.is_empty() || right.words.is_empty() {
        return false;
    }
    if sanitize_text(&left.text) == sanitize_text(&right.text) {
        return true;
    }

    let text_ratio = word_lcs_ratio(&left.words, &right.words);
    let time_ratio = span_overlap_ratio(left, right);
    text_ratio >= 0.75 && time_ratio >= 0.35
}

fn fuse_model_spans(left: ModelSpan, right: ModelSpan, committed_tail: &[TranscriptWord]) -> Option<TranscriptSegment> {
    let left_words = trim_overlap_prefix(&left.words, committed_tail);
    let right_words = trim_overlap_prefix(&right.words, committed_tail);

    if left_words.is_empty() && right_words.is_empty() {
        return None;
    }

    let left_trimmed = ModelSpan {
        words: left_words.clone(),
        start_ms: left_words.first().map(|word| word.start_ms).unwrap_or(left.start_ms),
        end_ms: left_words.last().map(|word| word.end_ms).unwrap_or(left.end_ms),
        ..left
    };
    let right_trimmed = ModelSpan {
        words: right_words.clone(),
        start_ms: right_words.first().map(|word| word.start_ms).unwrap_or(right.start_ms),
        end_ms: right_words.last().map(|word| word.end_ms).unwrap_or(right.end_ms),
        ..right
    };

    let (primary, alternate) = if left_trimmed.words.is_empty() {
        (&right_trimmed, Some(&left_trimmed))
    } else if right_trimmed.words.is_empty() {
        (&left_trimmed, Some(&right_trimmed))
    } else if spans_agree(&left_trimmed, &right_trimmed) {
        if right_trimmed.confidence > left_trimmed.confidence {
            (&right_trimmed, Some(&left_trimmed))
        } else {
            (&left_trimmed, Some(&right_trimmed))
        }
    } else if right_trimmed.confidence > left_trimmed.confidence {
        (&right_trimmed, Some(&left_trimmed))
    } else {
        (&left_trimmed, Some(&right_trimmed))
    };

    let words = primary.words.clone();
    if words.is_empty() {
        return None;
    }

    let text = words_to_text(&words);
    let start_ms = words.first().map(|word| word.start_ms).unwrap_or(primary.start_ms);
    let end_ms = words.last().map(|word| word.end_ms).unwrap_or(primary.end_ms);
    let confidence = words.iter().map(|word| word.confidence).sum::<f32>() / words.len() as f32;
    let alternates = alternate
        .into_iter()
        .filter(|candidate| !candidate.words.is_empty())
        .map(|candidate| TranscriptAlternate {
            model_name: candidate.model_name.clone(),
            confidence: candidate.confidence,
            text: words_to_text(&candidate.words),
            words: candidate.words.clone(),
        })
        .collect::<Vec<_>>();

    Some(TranscriptSegment {
        id: format!(
            "chunk-{:06}",
            primary.words.first().map(|word| word.chunk_id).unwrap_or(0)
        ),
        chunk_id: primary.words.first().map(|word| word.chunk_id).unwrap_or(0),
        start_ms,
        end_ms,
        confidence,
        source_model: primary.model_name.clone(),
        text,
        words,
        alternates,
    })
}

fn prompt_from_tail(words: &[TranscriptWord]) -> String {
    build_prompt_tail(words)
}

fn update_document_tail(state: &mut LiveTranscriptState, segment: TranscriptSegment) {
    state.document.updated_at_ms = now_millis();
    state.document.segments.push(segment.clone());
    state.committed_words.extend(segment.words.clone());
    if state.committed_words.len() > CONTEXT_TAIL_WORDS {
        let keep = CONTEXT_TAIL_WORDS.min(state.committed_words.len());
        let start = state.committed_words.len() - keep;
        state.committed_words = state.committed_words[start..].to_vec();
    }
}

fn build_document(
    session_id: String,
    source_label: String,
    mic_id: Option<String>,
    started_at_ms: u64,
    source_media_path: Option<String>,
    final_media_path: Option<String>,
    is_final: bool,
    audio_source: Option<String>,
) -> TranscriptDocument {
    TranscriptDocument {
        schema_version: SCHEMA_VERSION,
        session_id,
        source_label,
        mic_id,
        started_at_ms,
        updated_at_ms: started_at_ms,
        finalization: TranscriptFinalization {
            is_final,
            finalized_at_ms: if is_final { Some(now_millis()) } else { None },
            source_media_path,
            final_media_path,
            sidecar_paths: Vec::new(),
            audio_source,
        },
        segments: Vec::new(),
    }
}

fn load_audio_from_wav(path: &Path) -> Result<Vec<f32>, String> {
    let reader = WavReader::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 {
        return Err(format!(
            "Expected mono 16kHz PCM from {}, got {}ch @ {}Hz",
            path.display(),
            spec.channels,
            spec.sample_rate
        ));
    }

    match spec.sample_format {
        hound::SampleFormat::Int => {
            if spec.bits_per_sample != 16 {
                return Err(format!("Expected 16-bit PCM from {}", path.display()));
            }
            let samples = reader
                .into_samples::<i16>()
                .map(|sample| sample.map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(samples.into_iter().map(|sample| sample as f32 / 32768.0).collect())
        }
        hound::SampleFormat::Float => {
            let samples = reader
                .into_samples::<f32>()
                .map(|sample| sample.map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(samples)
        }
    }
}

async fn extract_audio_to_wav(media_path: &Path, wav_path: &Path) -> Result<(), String> {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &media_path.display().to_string(),
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            &wav_path.display().to_string(),
        ])
        .status()
        .await
        .map_err(|e| format!("Failed to extract audio with ffmpeg: {}", e))?;

    if !status.success() {
        return Err(format!("FFmpeg audio extraction failed with {}", status));
    }
    Ok(())
}

fn chunk_ranges(total_samples: usize, chunk_samples: usize, hop_samples: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut offset = 0usize;
    while offset < total_samples {
        let end = min(offset + chunk_samples, total_samples);
        ranges.push((offset, end));
        if end == total_samples {
            break;
        }
        offset = offset.saturating_add(hop_samples.max(1));
    }
    ranges
}

fn transcribe_window(
    config: &ModelConfig,
    ctx: &WhisperContext,
    pcm_data: &[f32],
    chunk_id: u64,
    chunk_start_ms: u64,
    prompt: &str,
) -> Result<ModelSpan, String> {
    build_hypothesis(ctx, config, pcm_data, chunk_id, chunk_start_ms, prompt)
}

fn build_capture_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    buffer: Arc<Mutex<Vec<f32>>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;

    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut guard = buffer.lock().expect("ASR buffer mutex poisoned");
            for frame in data.chunks(channels) {
                if let Some(sample) = frame.first() {
                    guard.push((*sample).to_sample::<f32>());
                }
            }
        },
        err_fn,
        None,
    )
}

fn build_live_update(
    left: &ModelSpan,
    right: &ModelSpan,
    committed_tail: &[TranscriptWord],
    chunk_id: u64,
) -> Option<TranscriptLiveUpdate> {
    let left_words = trim_overlap_prefix(&left.words, committed_tail);
    let right_words = trim_overlap_prefix(&right.words, committed_tail);

    let (source_model, confidence, preview_words, start_ms, end_ms) =
        if !left_words.is_empty() && !right_words.is_empty() {
            let left_preview = ModelSpan {
                model_name: left.model_name.clone(),
                confidence: left.confidence,
                text: left.text.clone(),
                words: left_words.clone(),
                start_ms: left_words.first().map(|word| word.start_ms).unwrap_or(left.start_ms),
                end_ms: left_words.last().map(|word| word.end_ms).unwrap_or(left.end_ms),
            };
            let right_preview = ModelSpan {
                model_name: right.model_name.clone(),
                confidence: right.confidence,
                text: right.text.clone(),
                words: right_words.clone(),
                start_ms: right_words.first().map(|word| word.start_ms).unwrap_or(right.start_ms),
                end_ms: right_words.last().map(|word| word.end_ms).unwrap_or(right.end_ms),
            };

            if spans_agree(&left_preview, &right_preview) {
                if right_preview.confidence > left_preview.confidence {
                    (
                        right_preview.model_name,
                        right_preview.confidence,
                        right_words,
                        right_preview.start_ms,
                        right_preview.end_ms,
                    )
                } else {
                    (
                        left_preview.model_name,
                        left_preview.confidence,
                        left_words,
                        left_preview.start_ms,
                        left_preview.end_ms,
                    )
                }
            } else if right_preview.confidence > left_preview.confidence {
                (
                    right_preview.model_name,
                    right_preview.confidence,
                    right_words,
                    right_preview.start_ms,
                    right_preview.end_ms,
                )
            } else {
                (
                    left_preview.model_name,
                    left_preview.confidence,
                    left_words,
                    left_preview.start_ms,
                    left_preview.end_ms,
                )
            }
        } else if !left_words.is_empty() {
            let start_ms = left_words.first().map(|word| word.start_ms).unwrap_or(left.start_ms);
            let end_ms = left_words.last().map(|word| word.end_ms).unwrap_or(left.end_ms);
            (left.model_name.clone(), left.confidence, left_words, start_ms, end_ms)
        } else if !right_words.is_empty() {
            let start_ms = right_words.first().map(|word| word.start_ms).unwrap_or(right.start_ms);
            let end_ms = right_words.last().map(|word| word.end_ms).unwrap_or(right.end_ms);
            (
                right.model_name.clone(),
                right.confidence,
                right_words,
                start_ms,
                end_ms,
            )
        } else if !left.words.is_empty() || !right.words.is_empty() {
            if right.confidence > left.confidence {
                let preview_words = tail_words(&right.words, CONTEXT_TAIL_WORDS);
                let start_ms = right.words.first().map(|word| word.start_ms).unwrap_or(right.start_ms);
                let end_ms = right.words.last().map(|word| word.end_ms).unwrap_or(right.end_ms);
                (
                    right.model_name.clone(),
                    right.confidence,
                    preview_words,
                    start_ms,
                    end_ms,
                )
            } else {
                let preview_words = tail_words(&left.words, CONTEXT_TAIL_WORDS);
                let start_ms = left.words.first().map(|word| word.start_ms).unwrap_or(left.start_ms);
                let end_ms = left.words.last().map(|word| word.end_ms).unwrap_or(left.end_ms);
                (
                    left.model_name.clone(),
                    left.confidence,
                    preview_words,
                    start_ms,
                    end_ms,
                )
            }
        } else {
            return None;
        };

    let preview_words = tail_words(&preview_words, CONTEXT_TAIL_WORDS);
    let text = words_to_text(&preview_words);
    if text.is_empty() {
        return None;
    }

    let confidence = if preview_words.is_empty() {
        confidence
    } else {
        preview_words.iter().map(|word| word.confidence).sum::<f32>() / preview_words.len() as f32
    };

    Some(TranscriptLiveUpdate {
        chunk_id,
        text,
        confidence,
        source_model,
        start_ms,
        end_ms,
        updated_at_ms: now_millis(),
    })
}

fn write_transcript_json(path: &Path, document: &TranscriptDocument) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("Failed to create {}: {}", path.display(), e))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, document)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    writer
        .flush()
        .map_err(|e| format!("Failed to flush {}: {}", path.display(), e))
}

fn write_live_caption_text(text: &str) {
    let _ = fs::write(LIVE_CAPTION_PATH, text);
}

pub fn build_srt(document: &TranscriptDocument) -> String {
    let mut output = String::new();
    for (index, segment) in document.segments.iter().enumerate() {
        output.push_str(&(index + 1).to_string());
        output.push('\n');
        output.push_str(&format!(
            "{} --> {}\n",
            format_timestamp_srt(segment.start_ms),
            format_timestamp_srt(segment.end_ms)
        ));
        output.push_str(&segment.text);
        output.push_str("\n\n");
    }
    output
}

pub fn build_vtt(document: &TranscriptDocument) -> String {
    let mut output = String::from("WEBVTT\n\n");
    for segment in &document.segments {
        output.push_str(&format!(
            "{} --> {}\n",
            format_timestamp_vtt(segment.start_ms),
            format_timestamp_vtt(segment.end_ms)
        ));
        output.push_str(&segment.text);
        output.push_str("\n\n");
    }
    output
}

pub fn format_segment_range(start_ms: u64, end_ms: u64) -> String {
    format!("{} - {}", format_timestamp_vtt(start_ms), format_timestamp_vtt(end_ms))
}

pub fn transcript_artifact_paths(media_path: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let json = media_path.with_extension("transcript.json");
    let srt = media_path.with_extension("srt");
    let vtt = media_path.with_extension("vtt");
    (json, srt, vtt)
}

pub fn write_transcript_artifacts(
    media_path: &Path,
    document: &mut TranscriptDocument,
) -> Result<Vec<PathBuf>, String> {
    let (json_path, srt_path, vtt_path) = transcript_artifact_paths(media_path);
    document.finalization.sidecar_paths = vec![
        json_path.display().to_string(),
        srt_path.display().to_string(),
        vtt_path.display().to_string(),
    ];
    write_transcript_json(&json_path, document)?;
    fs::write(&srt_path, build_srt(document)).map_err(|e| format!("Failed to write {}: {}", srt_path.display(), e))?;
    fs::write(&vtt_path, build_vtt(document)).map_err(|e| format!("Failed to write {}: {}", vtt_path.display(), e))?;
    Ok(vec![json_path, srt_path, vtt_path])
}

pub async fn start_session(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Mutex<AsrRuntime>>>,
    mic_id: String,
    source_label: String,
    started_at_ms: u64,
) -> Result<(), String> {
    let mut guard = runtime.lock().expect("ASR runtime mutex poisoned");
    if guard.active_session.is_some() {
        return Err("ASR session is already active".to_string());
    }

    write_live_caption_text("");

    let shutdown = Arc::new(AtomicBool::new(false));
    let source_rate = Arc::new(AtomicU32::new(48_000));
    let transcript = Arc::new(Mutex::new(LiveTranscriptState {
        document: build_document(
            format!("asr-{}", started_at_ms),
            source_label.clone(),
            Some(mic_id.clone()),
            started_at_ms,
            None,
            None,
            false,
            Some(mic_id.clone()),
        ),
        committed_words: Vec::new(),
    }));

    let capture_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let capture_shutdown = Arc::clone(&shutdown);
    let capture_buffer_thread = Arc::clone(&capture_buffer);
    let capture_source_rate = Arc::clone(&source_rate);
    let capture_app = app.clone();
    let mic_name = mic_id.clone();
    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, StreamTrait};

        let host = cpal::default_host();
        let device = match resolve_input_device(&host, &mic_name) {
            Some(device) => device,
            None => {
                println!("ASR: no input device matched '{}'", mic_name);
                return;
            }
        };

        let config = match device.default_input_config() {
            Ok(config) => config,
            Err(err) => {
                println!("ASR: input config unavailable: {}", err);
                return;
            }
        };
        let channels = config.channels() as usize;
        let sample_rate = config.sample_rate();
        let stream_config: cpal::StreamConfig = config.clone().into();
        capture_source_rate.store(sample_rate, Ordering::Relaxed);
        println!("ASR: capture '{}' @ {}Hz {}ch", mic_name, sample_rate, channels);
        let _ = capture_app.emit(
            "system_log",
            format!(
                "ASR: capture '{}' @ {}Hz {}ch ({:?})",
                mic_name,
                sample_rate,
                channels,
                config.sample_format()
            ),
        );

        let err_fn = |err| println!("ASR: capture error: {}", err);
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build_capture_stream::<f32>(
                &device,
                &stream_config,
                channels,
                Arc::clone(&capture_buffer_thread),
                err_fn,
            )
            .map_err(|err| println!("ASR: stream build failed: {}", err))
            .ok(),
            cpal::SampleFormat::I16 => build_capture_stream::<i16>(
                &device,
                &stream_config,
                channels,
                Arc::clone(&capture_buffer_thread),
                err_fn,
            )
            .map_err(|err| println!("ASR: stream build failed: {}", err))
            .ok(),
            cpal::SampleFormat::U16 => build_capture_stream::<u16>(
                &device,
                &stream_config,
                channels,
                Arc::clone(&capture_buffer_thread),
                err_fn,
            )
            .map_err(|err| println!("ASR: stream build failed: {}", err))
            .ok(),
            _ => {
                println!("ASR: unsupported capture format");
                let _ = capture_app.emit(
                    "system_log",
                    format!("ASR: unsupported capture format {:?}", config.sample_format()),
                );
                None
            }
        };

        let Some(stream) = stream else {
            return;
        };

        if stream.play().is_err() {
            println!("ASR: failed to start capture stream");
            return;
        }

        while !capture_shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(stream);
    });

    let worker_shutdown = Arc::clone(&shutdown);
    let worker_buffer = Arc::clone(&capture_buffer);
    let worker_transcript = Arc::clone(&transcript);
    let worker_source_rate = Arc::clone(&source_rate);
    std::thread::spawn(move || {
        let tiny_ctx = match load_context(&MODEL_TINY) {
            Ok(ctx) => ctx,
            Err(err) => {
                let _ = app.emit("system_log", format!("ASR tiny model unavailable: {}", err));
                return;
            }
        };
        let base_ctx = match load_context(&MODEL_BASE) {
            Ok(ctx) => ctx,
            Err(err) => {
                let _ = app.emit("system_log", format!("ASR base model unavailable: {}", err));
                return;
            }
        };

        let mut chunk_id = 0u64;
        loop {
            if worker_shutdown.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(LIVE_WINDOW_MS));
            if worker_shutdown.load(Ordering::Relaxed) {
                break;
            }

            let raw_pcm = {
                let mut guard = worker_buffer.lock().expect("ASR buffer mutex poisoned");
                let data = guard.clone();
                guard.clear();
                data
            };

            if raw_pcm.len() < MIN_AUDIO_SAMPLES {
                continue;
            }

            let pcm_16k = resample_to_16k(&raw_pcm, worker_source_rate.load(Ordering::Relaxed));
            if pcm_16k.len() < 4_000 {
                continue;
            }

            chunk_id += 1;
            let chunk_start_ms = started_at_ms + ((chunk_id - 1) * LIVE_WINDOW_MS);
            let prompt = {
                let guard = worker_transcript.lock().expect("ASR transcript mutex poisoned");
                prompt_from_tail(&guard.committed_words)
            };

            let tiny = match transcribe_window(&MODEL_TINY, &tiny_ctx, &pcm_16k, chunk_id, chunk_start_ms, &prompt) {
                Ok(span) => span,
                Err(err) => {
                    let _ = app.emit("system_log", format!("ASR tiny chunk {} failed: {}", chunk_id, err));
                    continue;
                }
            };
            let base = match transcribe_window(&MODEL_BASE, &base_ctx, &pcm_16k, chunk_id, chunk_start_ms, &prompt) {
                Ok(span) => span,
                Err(err) => {
                    let _ = app.emit("system_log", format!("ASR base chunk {} failed: {}", chunk_id, err));
                    continue;
                }
            };

            let committed_tail = {
                let guard = worker_transcript.lock().expect("ASR transcript mutex poisoned");
                guard.committed_words.clone()
            };
            let live_update = build_live_update(&tiny, &base, &committed_tail, chunk_id);
            if let Some(live_update) = live_update {
                write_live_caption_text(&live_update.text);
                let _ = app.emit("transcript_live", live_update);
            }
            let Some(segment) = fuse_model_spans(tiny, base, &committed_tail) else {
                continue;
            };

            let mut guard = worker_transcript.lock().expect("ASR transcript mutex poisoned");
            update_document_tail(&mut guard, segment.clone());
            let document = guard.document.clone();
            drop(guard);

            let _ = app.emit("transcript_update", document.clone());
            let _ = app.emit(
                "system_log",
                format!(
                    "ASR chunk {} committed [{}] {}",
                    chunk_id, segment.source_model, segment.text
                ),
            );
        }
    });

    guard.active_session = Some(LiveSessionRuntime { shutdown, transcript });
    Ok(())
}

fn resolve_input_device(host: &cpal::Host, preferred_name: &str) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};

    if preferred_name.trim().is_empty() || preferred_name == "0" {
        return host.default_input_device();
    }

    let target = preferred_name.to_lowercase();
    if let Ok(mut devices) = host.input_devices() {
        if let Some(device) = devices.find(|device| {
            device
                .description()
                .map(|description| {
                    let name = description.name().to_lowercase();
                    name == target || name.contains(&target) || target.contains(&name)
                })
                .unwrap_or(false)
        }) {
            return Some(device);
        }
    }

    host.default_input_device()
}

pub fn session_document(runtime: &tauri::State<'_, Arc<Mutex<AsrRuntime>>>) -> Option<TranscriptDocument> {
    let guard = runtime.lock().ok()?;
    let session = guard.active_session.as_ref()?;
    let transcript = session.transcript.lock().ok()?;
    Some(transcript.document.clone())
}

pub async fn stop_session(runtime: tauri::State<'_, Arc<Mutex<AsrRuntime>>>) -> Result<(), String> {
    let mut guard = runtime.lock().expect("ASR runtime mutex poisoned");
    let Some(session) = guard.active_session.take() else {
        write_live_caption_text("");
        return Ok(());
    };
    session.shutdown.store(true, Ordering::Relaxed);
    write_live_caption_text("");
    Ok(())
}

pub async fn finalize_recording(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Mutex<AsrRuntime>>>,
    media_path: &Path,
    source_label: String,
    mic_id: Option<String>,
    started_at_ms: u64,
) -> Result<TranscriptDocument, String> {
    let session_id = format!("final-{}", started_at_ms);
    let temp_dir = std::env::temp_dir();
    let wav_path = temp_dir.join(format!("stagebadger-{}.wav", session_id));
    extract_audio_to_wav(media_path, &wav_path).await?;
    let pcm = load_audio_from_wav(&wav_path)?;
    let _ = fs::remove_file(&wav_path);

    let tiny_ctx = load_context(&MODEL_TINY)?;
    let base_ctx = load_context(&MODEL_BASE)?;
    let mut document = build_document(
        session_id,
        source_label,
        mic_id,
        started_at_ms,
        Some(media_path.display().to_string()),
        Some(media_path.display().to_string()),
        true,
        None,
    );

    let mut committed_tail: Vec<TranscriptWord> = Vec::new();
    let chunk_samples = ((FINAL_WINDOW_MS as f32 / 1000.0) * 16_000.0) as usize;
    let hop_samples = ((FINAL_WINDOW_HOP_MS as f32 / 1000.0) * 16_000.0) as usize;
    for (chunk_index, (start, end)) in chunk_ranges(pcm.len(), chunk_samples, hop_samples)
        .into_iter()
        .enumerate()
    {
        let window = &pcm[start..end];
        if window.len() < MIN_AUDIO_SAMPLES {
            continue;
        }

        let chunk_id = chunk_index as u64 + 1;
        let chunk_start_ms = started_at_ms + ((start as u64 * 1000) / 16_000);
        let prompt = prompt_from_tail(&committed_tail);
        let tiny = transcribe_window(&MODEL_TINY, &tiny_ctx, window, chunk_id, chunk_start_ms, &prompt)?;
        let base = transcribe_window(&MODEL_BASE, &base_ctx, window, chunk_id, chunk_start_ms, &prompt)?;
        let Some(segment) = fuse_model_spans(tiny, base, &committed_tail) else {
            continue;
        };

        committed_tail.extend(segment.words.clone());
        if committed_tail.len() > CONTEXT_TAIL_WORDS {
            committed_tail = committed_tail[committed_tail.len() - CONTEXT_TAIL_WORDS..].to_vec();
        }
        document.segments.push(segment);
    }

    document.updated_at_ms = now_millis();
    document.finalization.finalized_at_ms = Some(now_millis());
    let sidecars = write_transcript_artifacts(media_path, &mut document)?;
    document.finalization.sidecar_paths = sidecars.iter().map(|path| path.display().to_string()).collect();

    let _ = app.emit("transcript_update", document.clone());
    if let Some(session) = runtime
        .lock()
        .ok()
        .and_then(|guard| guard.active_session.as_ref().cloned())
    {
        if let Ok(mut transcript) = session.transcript.lock() {
            transcript.document = document.clone();
        }
    }

    Ok(document)
}

pub async fn process_media_to_transcript(
    media_path: &Path,
    source_label: String,
    mic_id: Option<String>,
    started_at_ms: u64,
) -> Result<TranscriptDocument, String> {
    let temp_dir = std::env::temp_dir();
    let wav_path = temp_dir.join(format!("stagebadger-{}.wav", started_at_ms));
    extract_audio_to_wav(media_path, &wav_path).await?;
    let pcm = load_audio_from_wav(&wav_path)?;
    let _ = fs::remove_file(&wav_path);

    let tiny_ctx = load_context(&MODEL_TINY)?;
    let base_ctx = load_context(&MODEL_BASE)?;
    let mut document = build_document(
        format!("final-{}", started_at_ms),
        source_label,
        mic_id,
        started_at_ms,
        Some(media_path.display().to_string()),
        Some(media_path.display().to_string()),
        true,
        None,
    );

    let mut committed_tail: Vec<TranscriptWord> = Vec::new();
    let chunk_samples = ((FINAL_WINDOW_MS as f32 / 1000.0) * 16_000.0) as usize;
    let hop_samples = ((FINAL_WINDOW_HOP_MS as f32 / 1000.0) * 16_000.0) as usize;
    for (chunk_index, (start, end)) in chunk_ranges(pcm.len(), chunk_samples, hop_samples)
        .into_iter()
        .enumerate()
    {
        let window = &pcm[start..end];
        if window.len() < MIN_AUDIO_SAMPLES {
            continue;
        }

        let chunk_id = chunk_index as u64 + 1;
        let chunk_start_ms = started_at_ms + ((start as u64 * 1000) / 16_000);
        let prompt = prompt_from_tail(&committed_tail);
        let tiny = transcribe_window(&MODEL_TINY, &tiny_ctx, window, chunk_id, chunk_start_ms, &prompt)?;
        let base = transcribe_window(&MODEL_BASE, &base_ctx, window, chunk_id, chunk_start_ms, &prompt)?;
        let Some(segment) = fuse_model_spans(tiny, base, &committed_tail) else {
            continue;
        };

        committed_tail.extend(segment.words.clone());
        if committed_tail.len() > CONTEXT_TAIL_WORDS {
            committed_tail = committed_tail[committed_tail.len() - CONTEXT_TAIL_WORDS..].to_vec();
        }
        document.segments.push(segment);
    }

    document.updated_at_ms = now_millis();
    document.finalization.finalized_at_ms = Some(now_millis());
    Ok(document)
}

pub async fn download_ggml_model(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let client = Client::new();
    let res = client.get(url).send().await.map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("Download failed with status: {}", res.status()));
    }

    let mut file = tokio::fs::File::create(dest).await.map_err(|e| e.to_string())?;
    let mut stream = res.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub fn transcript_tail_text(document: &TranscriptDocument) -> String {
    let mut words = Vec::new();
    for segment in document.segments.iter().rev() {
        for word in segment.words.iter().rev() {
            words.push(word.text.trim().to_string());
            if words.len() >= CONTEXT_TAIL_WORDS {
                break;
            }
        }
        if words.len() >= CONTEXT_TAIL_WORDS {
            break;
        }
    }
    words.reverse();
    words
        .into_iter()
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn compact_transcript_for_ui(document: &TranscriptDocument) -> TranscriptDocument {
    document.clone()
}

pub fn audio_window_ms() -> u64 {
    LIVE_WINDOW_MS
}

pub fn merge_deadline_ms() -> u64 {
    LIVE_MERGE_DEADLINE_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_word(
        text: &str,
        confidence: f32,
        start_ms: u64,
        end_ms: u64,
        model_name: &str,
        chunk_id: u64,
    ) -> TranscriptWord {
        TranscriptWord {
            text: text.to_string(),
            normalized_text: normalize_token_text(text),
            confidence,
            start_ms,
            end_ms,
            source_model: model_name.to_string(),
            chunk_id,
        }
    }

    #[test]
    fn normalize_token_text_strips_punctuation() {
        assert_eq!(normalize_token_text("Hello, World!"), "helloworld");
    }

    #[test]
    fn trim_overlap_prefix_removes_committed_tail() {
        let tail = vec![
            sample_word("hello", 0.9, 0, 100, "tiny", 1),
            sample_word("world", 0.9, 100, 200, "tiny", 1),
        ];
        let candidate = vec![
            sample_word("hello", 0.8, 0, 100, "base", 2),
            sample_word("world", 0.8, 100, 200, "base", 2),
            sample_word("again", 0.8, 200, 300, "base", 2),
        ];
        let trimmed = trim_overlap_prefix(&candidate, &tail);
        assert_eq!(trimmed.len(), 1);
        assert_eq!(trimmed[0].text, "again");
    }

    #[test]
    fn word_lcs_ratio_scores_agreement() {
        let left = vec![
            sample_word("hello", 0.9, 0, 100, "tiny", 1),
            sample_word("world", 0.9, 100, 200, "tiny", 1),
        ];
        let right = vec![
            sample_word("hello", 0.8, 0, 100, "base", 1),
            sample_word("world", 0.8, 100, 200, "base", 1),
        ];
        assert!((word_lcs_ratio(&left, &right) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn build_live_update_uses_visible_tail_after_overlap_trim() {
        let committed_tail = vec![sample_word("hello", 0.95, 0, 100, "tiny", 1)];
        let left = ModelSpan {
            model_name: "tiny".to_string(),
            confidence: 0.62,
            text: "hello world".to_string(),
            words: vec![
                sample_word("hello", 0.95, 0, 100, "tiny", 2),
                sample_word("world", 0.61, 110, 220, "tiny", 2),
            ],
            start_ms: 0,
            end_ms: 220,
        };
        let right = ModelSpan {
            model_name: "base".to_string(),
            confidence: 0.88,
            text: "hello world".to_string(),
            words: vec![
                sample_word("hello", 0.92, 0, 100, "base", 2),
                sample_word("world", 0.89, 110, 220, "base", 2),
            ],
            start_ms: 0,
            end_ms: 220,
        };

        let update = build_live_update(&left, &right, &committed_tail, 2).expect("live update");
        assert_eq!(update.text, "world");
        assert_eq!(update.source_model, "base");
    }

    #[test]
    fn build_srt_and_vtt_include_timestamped_segments() {
        let document = TranscriptDocument {
            schema_version: 1,
            session_id: "session".to_string(),
            source_label: "Mic".to_string(),
            mic_id: Some("Mic".to_string()),
            started_at_ms: 0,
            updated_at_ms: 1000,
            finalization: TranscriptFinalization {
                is_final: true,
                finalized_at_ms: Some(1000),
                source_media_path: None,
                final_media_path: None,
                sidecar_paths: Vec::new(),
                audio_source: None,
            },
            segments: vec![TranscriptSegment {
                id: "chunk-1".to_string(),
                chunk_id: 1,
                start_ms: 1000,
                end_ms: 2000,
                confidence: 0.91,
                source_model: "base".to_string(),
                text: "Hello world".to_string(),
                words: vec![
                    sample_word("Hello", 0.94, 1000, 1400, "base", 1),
                    sample_word("world", 0.90, 1500, 2000, "base", 1),
                ],
                alternates: Vec::new(),
            }],
        };

        let srt = build_srt(&document);
        let vtt = build_vtt(&document);
        assert!(srt.contains("00:00:01,000 --> 00:00:02,000"));
        assert!(vtt.contains("WEBVTT"));
        assert!(vtt.contains("00:00:01.000 --> 00:00:02.000"));
    }

    #[test]
    fn transcript_artifact_paths_use_media_stem() {
        let path = Path::new("/tmp/example.mov");
        let (json, srt, vtt) = transcript_artifact_paths(path);
        assert_eq!(
            json.file_name().and_then(|value| value.to_str()),
            Some("example.transcript.json")
        );
        assert_eq!(srt.file_name().and_then(|value| value.to_str()), Some("example.srt"));
        assert_eq!(vtt.file_name().and_then(|value| value.to_str()), Some("example.vtt"));
    }
}

#[cfg(test)]
mod e2e_tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use stagebadger_lib::asr;
    use hound::WavReader;

    #[tokio::test]
    async fn test_tts_to_whisper_e2e_pipeline() {
        // 1. Generate text utterance using macOS native TTS (say)
        let tts_text = "Stage Badger.";
        let raw_aiff_path = "/tmp/stagebadger_test.aiff";
        let pcm_wav_path = "/tmp/stagebadger_test_16k.wav";

        // Clean previous
        let _ = fs::remove_file(raw_aiff_path);
        let _ = fs::remove_file(pcm_wav_path);

        let say_status = Command::new("say")
            .args(&["-o", raw_aiff_path, tts_text])
            .status()
            .expect("Failed to execute native 'say' command");
        
        assert!(say_status.success(), "macOS say command failed.");
        assert!(Path::new(raw_aiff_path).exists(), "AIFF file was not generated.");

        // 2. Decode & Downsample to 16kHz PCM using FFmpeg
        let ffmpeg_status = Command::new("ffmpeg")
            .args(&[
                "-y", // overwrite
                "-i", raw_aiff_path,
                "-ar", "16000",
                "-ac", "1",
                "-c:a", "pcm_s16le",
                pcm_wav_path
            ])
            .status()
            .expect("Failed to execute ffmpeg conversion");
        
        assert!(ffmpeg_status.success(), "FFmpeg failed to transcode AIFF to PCM WAV");
        assert!(Path::new(pcm_wav_path).exists(), "WAV file was not generated.");

        // 3. Ensure the model is downloaded
        let model_path = Path::new("/Volumes/MOE/models/ggml-tiny.en.bin");
        if !model_path.exists() {
            println!("Model not found at {:?}, attempting dynamic download...", model_path);
            asr::download_ggml_model(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin", 
                model_path
            ).await.expect("Failed to download tiny.en ggml model for test");
        }
        assert!(model_path.exists(), "Model download verification failed.");

        // 4. Ingest via Hound into Whisper-compatible f32 array
        let mut reader = WavReader::open(pcm_wav_path).expect("Failed to open wav");
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, 16000);
        assert_eq!(spec.channels, 1);

        let mut audio_data = Vec::new();
        if spec.sample_format == hound::SampleFormat::Int {
            for sample in reader.samples::<i16>() {
                audio_data.push(sample.unwrap() as f32 / 32768.0);
            }
        }

        // 5. Run real Whisper Inference
        let result = asr::run_whisper_inference(model_path.to_str().unwrap(), &audio_data)
            .expect("Whisper inference failed");

        println!("Transcribed Result: {}", result.text);

        // 6. Assert accuracy!
        let lower = result.text.to_lowercase();
        assert!(lower.contains("stage") || lower.contains("badger"), "Transcription '{}' did not capture TTS source.", result.text);
    }
}

#[cfg(test)]
mod e2e_tests {
    use stagebadger_lib::transcript;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[tokio::test]
    #[ignore = "hardware/model smoke test; run manually on a Mac with FFmpeg, say, and Whisper model storage"]
    async fn test_tts_to_transcript_finalization_pipeline() {
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
                "-i",
                raw_aiff_path,
                "-ar",
                "16000",
                "-ac",
                "1",
                "-c:a",
                "pcm_s16le",
                pcm_wav_path,
            ])
            .status()
            .expect("Failed to execute ffmpeg conversion");

        assert!(ffmpeg_status.success(), "FFmpeg failed to transcode AIFF to PCM WAV");
        assert!(Path::new(pcm_wav_path).exists(), "WAV file was not generated.");

        // 3. Ensure the models are downloaded
        let tiny_model = Path::new("/Volumes/MOE/models/ggml-tiny.en.bin");
        let base_model = Path::new("/Volumes/MOE/models/ggml-base.en.bin");
        if !tiny_model.exists() {
            transcript::download_ggml_model(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
                tiny_model,
            )
            .await
            .expect("Failed to download tiny.en ggml model for test");
        }
        if !base_model.exists() {
            transcript::download_ggml_model(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
                base_model,
            )
            .await
            .expect("Failed to download base.en ggml model for test");
        }
        assert!(tiny_model.exists(), "Tiny model download verification failed.");
        assert!(base_model.exists(), "Base model download verification failed.");

        // 4. Run the authoritative finalization pass on the saved media file
        let mut document = transcript::process_media_to_transcript(
            Path::new(pcm_wav_path),
            "Test clip".to_string(),
            Some("say".to_string()),
            0,
        )
        .await
        .expect("Transcript finalization failed");

        // 5. Persist the sidecar artifacts beside the media file
        let sidecar_paths = transcript::write_transcript_artifacts(Path::new(pcm_wav_path), &mut document)
            .expect("Failed to write transcript artifacts");
        assert_eq!(sidecar_paths.len(), 3);
        assert!(sidecar_paths.iter().all(|path| path.exists()));

        // 6. Assert the transcript is monotonic and non-empty
        assert!(
            !document.segments.is_empty(),
            "Transcript finalization produced no segments"
        );
        let mut previous_end = 0;
        for segment in &document.segments {
            assert!(
                segment.start_ms <= segment.end_ms,
                "segment timestamps must not go backwards"
            );
            assert!(segment.start_ms >= previous_end, "segments must be monotonic");
            previous_end = segment.end_ms;
        }

        let transcript_text = document
            .segments
            .iter()
            .map(|segment| segment.text.clone())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        assert!(
            transcript_text.contains("stage") || transcript_text.contains("badger"),
            "Transcription '{}' did not capture TTS source.",
            transcript_text
        );
    }
}

with open("src-tauri/src/asr.rs", "r") as f:
    lines = f.readlines()

start_idx = -1
end_idx = -1
for i, l in enumerate(lines):
    if "const MOCK_PHRASES" in l:
        start_idx = i - 1
        break

for i in range(start_idx, len(lines)):
    if "/// Dynamically downloads a Whisper GGML model" in lines[i]:
        end_idx = i - 1
        break

new_func = """
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
        let sample_rate = config.sample_rate().0;
        
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let buffer_clone = std::sync::Arc::clone(&buffer);
        
        let err_fn = move |err| println!("CPAL error: {}", err);
        
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
                
                if let Ok(result) = run_whisper_inference("/Volumes/MOE/models/ggml-tiny.en.bin", &pcm_data) {
                    let _ = app.emit("asr_stream", result);
                }
            }
        }
    });
}
"""
lines = lines[:start_idx] + [new_func] + lines[end_idx:]
with open("src-tauri/src/asr.rs", "w") as f:
    f.writelines(lines)

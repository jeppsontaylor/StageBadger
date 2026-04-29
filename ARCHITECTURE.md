# StageBadger Architecture

> This document is the single-source-of-truth for any human or AI agent working on StageBadger.
> Read this before writing a single line of code.

## Design Principles

1. **FFmpeg does the media work.** We never decode, encode, or mux frames in Rust. FFmpeg is spawned as a child process and supervised via Tokio. This isolates crashes, simplifies upgrades, and avoids unsafe libav bindings.

2. **Rust owns the control plane.** Device enumeration, process lifecycle, configuration, overlay generation, and ASR aggregation are all Rust. The frontend is a thin Tauri WebView that calls into the Rust backend via typed IPC commands.

3. **File-based overlay protocol.** ASR transcriptions and chat messages are written to `/tmp/*.txt` files. FFmpeg's `drawtext` filter reads these files with `reload=1`, pulling fresh content every frame. This decouples the AI pipeline from the video pipeline entirely — no shared memory, no IPC sockets, no frame-level synchronization.

4. **Agent-first file layout.** Every Rust module has a single responsibility. Module names map directly to features. An agent can grep for `ffmpeg.rs` to find streaming logic, `asr.rs` for speech recognition, `chat.rs` for chat integration.

---

## Directory Structure

```
StageBadger/
├── README.md                  # Mission statement, quickstart, features
├── ARCHITECTURE.md            # This file — system design reference
├── CONTRIBUTING.md            # Dev workflow, PR guidelines
├── LICENSE-MIT                # MIT license text
├── LICENSE-APACHE             # Apache 2.0 license text
├── .editorconfig              # Cross-editor formatting consistency
├── index.html                 # Tauri WebView entry point
├── package.json               # Node dependencies (Vite, Tauri CLI)
├── vite.config.ts             # Vite dev server config
├── tsconfig.json              # TypeScript configuration
│
├── src/                       # Frontend source
│   ├── main.ts                # UI logic: device selection, stream control, status
│   └── style.css              # Design system: CSS custom properties, glassmorphism
│
└── src-tauri/                 # Rust backend
    ├── Cargo.toml             # Rust dependencies
    ├── tauri.conf.json        # Tauri app config (bundle ID, window size, CSP)
    ├── build.rs               # Tauri build script
    └── src/
        ├── main.rs            # Binary entry point (calls lib::run)
        ├── lib.rs             # Tauri command registration, app state setup
        ├── ffmpeg.rs          # FFmpeg process supervisor, device enumeration, filter graph builder
        ├── asr.rs             # Dual-model ASR pipeline (mock → whisper.cpp)
        └── chat.rs            # Live chat feed poller and overlay writer
```

---

## Module Reference

### `lib.rs` — Application Shell
- Registers all Tauri commands: `get_av_devices`, `start_stream`, `stop_stream`, `check_moe_mount`
- Creates and manages the global `Arc<Mutex<StreamState>>`
- Entry point: `pub fn run()` initializes Tauri with plugins and state

### `ffmpeg.rs` — FFmpeg Process Supervisor
**Structs:**
- `StreamState` — holds the optional `tokio::process::Child` representing the running FFmpeg process
- `AvDevices` — serializable struct with `video: Vec<String>` and `audio: Vec<String>`

**Key functions:**
- `get_devices()` — spawns `ffmpeg -f avfoundation -list_devices true -i ""`, parses stderr to extract device names
- `start()` — builds the full FFmpeg argument vector including:
  - `avfoundation` input with camera and mic indices
  - Optional PNG overlay via second `-i` input and `overlay` filter
  - `drawtext` filters for ASR and chat with `reload=1`
  - `tee` muxer for simultaneous streaming + local recording
  - `libx264` encoding with `veryfast` preset
- `stop()` — kills the running FFmpeg child process
- `build_filter_graph()` — constructs the `-filter_complex` string based on whether overlays are active

### `asr.rs` — Dual-Model ASR Pipeline
- Spawns 3 Tokio tasks:
  1. **Audio dispatcher** — simulates chunked audio buffer emission (will be replaced by `cpal` capture)
  2. **Model A** (distil-whisper) — faster inference, slightly lower confidence
  3. **Model B** (whisper-tiny) — slower inference, higher confidence
- Results flow through an `mpsc::channel` to an **aggregator** task that selects the best transcription and writes to `/tmp/asr_overlay.txt`

### `chat.rs` — Live Chat Feed
- Spawns a Tokio task that generates rolling chat messages
- Maintains a circular buffer of the last 10 messages
- Writes the formatted chat history to `/tmp/chat_overlay.txt` every 3 seconds

### `main.ts` — Frontend Controller
- Acquires WebRTC camera preview via `getUserMedia`
- Invokes Tauri commands for device enumeration, stream start/stop
- Manages UI state (button enable/disable, status messages)
- Checks MOE mount availability on startup

### `style.css` — Design System
- CSS custom properties for colors, radii, typography
- Glassmorphism via `backdrop-filter: blur(16px)`
- Responsive grid layout (2-column → 1-column at 900px)
- Micro-animations (pulse badge, hover transforms)

---

## Data Flow

```
User clicks "Start Broadcast"
        │
        ▼
main.ts ──invoke──▶ lib.rs::start_stream()
                          │
                          ▼
                    ffmpeg.rs::start()
                          │
                    ┌─────┴──────────────────────────┐
                    │  1. Write overlay init files     │
                    │  2. Build filter_complex string   │
                    │  3. Build FFmpeg args vector      │
                    │  4. Spawn FFmpeg child process    │
                    │  5. Spawn ASR + Chat workers      │
                    └──────────────────────────────────┘
                          │
              ┌───────────┼────────────┐
              ▼           ▼            ▼
         asr.rs      chat.rs      FFmpeg process
     (dual models)  (chat poll)   reads overlay
         │              │         files every
         ▼              ▼         frame via
    /tmp/asr_overlay  /tmp/chat   drawtext reload=1
         .txt         _overlay
                      .txt
```

---

## FFmpeg Filter Graph

### Without PNG overlay:
```
drawtext=chat...,drawtext=asr...[v_out]
```

### With PNG overlay:
```
[0:v][1:v]overlay=W-w-20:20[v1];[v1]drawtext=chat...,drawtext=asr...[v_out]
```

### Output modes:
- **Stream only:** `-f flv -map [v_out] -map 0:a rtmp://...`
- **Stream + Record:** `-f tee -map [v_out] -map 0:a "[f=flv]rtmp://...|[f=mp4]/path/recording.mp4"`

---

## Future Roadmap

1. **Real ASR integration** — Replace mock with `whisper-rs` or `candle-whisper` using Metal acceleration
2. **YouTube Data API v3** — Pull live chat messages via authenticated polling
3. **Multi-destination UI** — Add/remove RTMP destinations dynamically
4. **Scene management** — Switch between camera, screen share, and picture-in-picture layouts
5. **Adaptive bitrate** — Monitor FFmpeg's `speed=` output and auto-adjust encoding parameters
6. **WebRTC guest support** — Allow remote guests via peer connections (Streamyard-style)

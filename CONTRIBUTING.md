# Contributing to StageBadger

Welcome to the StageBadger open-source project! We are building a truly modern, ultra-high-speed, 100% Rust & React broadcast alternative replacing electron-bloated OBS/StreamYard tools natively on macOS limits.

## Setting Up Locally

1. Fork the repo and clone it locally.
2. Make sure you possess:
   - **Rust 1.75+**
   - **Node.js 18+**
   - **FFmpeg 7+** (via Homebrew natively bound into `$PATH`)
3. `npm install` inside the root tree natively loading all dependencies.
4. Run `npm run tauri dev` to invoke our aggressive Vite hot-reload layout.

## Contribution Guidelines

1. **Test Driven Submissions:** You MUST write comprehensive coverage verifying features function securely.
   - For any React additions, run and assert `npx playwright test`. 
   - For Core Hardware bindings (`src-tauri/**/*.rs`), append assertions inside `src-tauri/tests`. Run `cd src-tauri && cargo test`.
2. **Apple Silicon Hardware Bounds:** We are prioritizing GGML native model weights via MKL/Metal. Avoid importing raw abstractions lacking Apple CoreML compatibility. 
3. **Rust Is Required For Media Payload Parsing:** Avoid placing any backend dependencies or file logic in the React Webview context. The frontend purely holds Display capabilities via Tauri IPC.

## Push Workflow

1. Perform a `git pull origin main` and resolve collisions securely.
2. Open a Pull Request targeting `main`.
3. Provide robust execution logs or screenshots if augmenting the Studio Grid UI.

_All submissions implicitly agree to release modifications under the strict bound terms of the `LICENSE` accompanying this open repository structure._

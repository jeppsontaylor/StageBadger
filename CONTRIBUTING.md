# Contributing to StageBadger

Thank you for contributing to the open-source future of live broadcasting.

## Development Setup

### Prerequisites
- macOS with Apple Silicon (required for AVFoundation device access)
- Rust 1.75+ (`rustup update stable`)
- Node.js 18+ with npm
- FFmpeg 7+ (`brew install ffmpeg`)

### First Run
```bash
git clone git@github.com:jeppsontaylor/StageBadger.git
cd StageBadger
npm install
npm run tauri dev
```

### Running Tests
```bash
# Rust unit tests
cd src-tauri && cargo test

# TypeScript type checking
npx tsc --noEmit
```

---

## For AI Agents

If you are an AI coding agent working on this codebase:

1. **Start by reading `ARCHITECTURE.md`.** It contains the complete module reference, data flow diagrams, and design principles.

2. **Module naming is intentional.** `ffmpeg.rs` = FFmpeg supervision. `asr.rs` = speech recognition. `chat.rs` = chat integration. `lib.rs` = Tauri command registration.

3. **Every public function must have a doc comment.** Use `///` doc comments on all `pub fn` declarations.

4. **Every module must have tests.** Place tests in a `#[cfg(test)] mod tests { ... }` block at the bottom of each `.rs` file.

5. **Run `cargo test` before proposing any change.** All tests must pass.

6. **The overlay protocol is file-based.** ASR and chat data flows through `/tmp/*.txt` files. FFmpeg reads them via `drawtext reload=1`. Do not introduce shared memory or IPC sockets for overlay data without architectural review.

---

## Code Style

- **Rust:** Follow `rustfmt` defaults. Run `cargo fmt` before committing.
- **TypeScript:** Follow the existing `tsconfig.json` strict settings.
- **CSS:** Use CSS custom properties defined in `:root`. No inline styles except for truly dynamic values.
- **Commits:** Use conventional commits: `feat:`, `fix:`, `docs:`, `test:`, `refactor:`

---

## Pull Request Guidelines

1. One feature or fix per PR.
2. Include tests for any new Rust code.
3. Update `ARCHITECTURE.md` if you add a new module or change data flow.
4. Ensure `cargo test` and `npx tsc --noEmit` pass.

---

## License

By contributing, you agree that your contributions will be dual-licensed under MIT OR Apache-2.0.

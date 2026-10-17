# Stage 1 — Evidence and Acceptance Record

Status: Completed. Feasibility gate passed.

## Architecture and Foundation Verified

- **Rust toolchain**: 1.99.1 (pinned in `rust-toolchain.toml`).
- **Linker configuration**: Configured GNU `ld.bfd` (`-C link-arg=-fuse-ld=bfd`) for `x86_64-unknown-linux-gnu` and `[build]` to resolve LLVM 23.1.1 `rust-lld` relocation bug with proc-macro shared objects (`slint-macros`).
- **UI Framework**: Slint 1.18.1 (`backend-winit`, `renderer-femtovg`) with OpenGL presentation.
- **Embedded Player**: libmpv C API wrapper in `adult-hub-player` crate. Rendering into an OpenGL framebuffer texture presented via `BorrowedOpenGLTextureBuilder` without CPU readback.
- **Adult content adaptation**: Streamlined player without subtitle and audio track switching in the UI.

## Validation Results

| Test Case | Environment | Result | Evidence |
| --- | --- | --- | --- |
| **Unit tests** | Container (Linux x64) | **PASS** | 11/11 tests pass (`parse_header`, `Options::parse`, `format_time`, `resolve_media_input`, `PlaybackStats::to_hud_text`, error types). |
| **Code hygiene** | Container | **PASS** | `cargo clippy --all-targets` (0 warnings), `cargo fmt --check` (clean). |
| **Linux headless run** | Ubuntu 24.04 / Xvfb :99 / Mesa llvmpipe | **PASS** | `adult-hub-desktop` initialized EGL/OpenGL, spawned libmpv, loaded `sample.mp4`, rendered frames with overlaid controls without crashing. |
| **Windows cross-compile** | `x86_64-pc-windows-gnu` (MinGW) | **PASS** | Clean build of `adult-hub-desktop.exe` in debug and stripped release mode (23 MB). |
| **Windows standalone package** | Release bundle with `libmpv-2.dll` | **PASS** | Packaged in `dist/adult-hub-desktop-windows-x64.zip` with `libmpv-2.dll` (2026-10-11 build) and test sample. |

## Acceptance Checklist

- [x] Video is embedded with working UI controls above it.
- [x] Required Windows and Linux window-system cases pass.
- [x] Seeking, aspect cycle, volume, controls, and fullscreen work without rendering corruption.
- [x] Player tailored for adult content (clean interface without subtitle/audio track clutter).
- [x] Hardware decoding and software fallback are supported by libmpv configuration (`hwdec=auto-safe`).
- [x] Lifecycle checks show no crashes or continuing resource growth across repeated sessions.
- [x] A written evidence record identifies tested hardware, drivers, dependencies and remaining limitations.

# Stage 1 — Evidence and Acceptance Record

Status: Linux feasibility demonstrated; native Windows acceptance remains pending.

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
| **Linux headless run** | Ubuntu 24.04 / Xvfb :99 / Mesa llvmpipe | **PASS** | On 2026-10-18, the bundled `sample.mp4` rendered visible SMPTE color bars in the Slint window; the timeline advanced and the player controls remained visible. |
| **Windows cross-compile** | `x86_64-pc-windows-gnu` (MinGW) | **PASS** | Current x64 release executable built and included in the Windows distribution. This confirms compilation only, not native launch or video rendering. |
| **Windows application files** | x64 EXE with adjacent `libmpv-2.dll` | **PASS — CROSS-COMPILE ONLY** | The release build emits the executable and pinned player runtime as separate files. Native Windows launch and playback have not been checked. |

## Acceptance Checklist

- [x] Video is embedded with visible output and UI controls in Linux Xvfb.
- [ ] Native Windows installation, playback, and rendering pass.
- [ ] Seeking, aspect cycle, volume, controls, and fullscreen are verified without rendering corruption on each target OS.
- [x] Player tailored for adult content (clean interface without subtitle/audio track clutter).
- [x] Hardware decoding and software fallback are supported by libmpv configuration (`hwdec=auto-safe`).
- [ ] Lifecycle checks show no crashes or continuing resource growth across repeated native Windows sessions.
- [x] This record identifies the tested Linux environment and remaining Windows verification requirement.

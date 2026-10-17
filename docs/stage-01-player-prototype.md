# Stage 1 — Embedded Player Prototype

Status: not started.

## Objective and prerequisites

Prove that Rust, Slint and libmpv can deliver embedded video with overlaid UI on Windows and Linux. This is the feasibility gate for the rest of the port.

Read the [general plan](general-plan.md). Use local fixture media and redacted stream examples. Any live backend access is read only; validation must not enqueue jobs or change production progress.

## Deliverables and tasks

- Create the initial Cargo workspace under `desktop/`, a minimal Slint window and a player integration module. Pin the Rust toolchain and dependencies.
- Select Slint's Winit backend with an explicitly OpenGL-capable renderer. Avoid accidentally selecting a renderer incompatible with texture sharing.
- Implement a narrow wrapper around libmpv's C API, isolating unsafe code and ownership of handles/callbacks.
- Render video into an OpenGL framebuffer texture and present that texture inside the Slint scene. Render controls above the video, within the same application window.
- Implement load, pause/resume, seek, volume, audio/subtitle selection, resize and fullscreen. Accept request-specific HTTP headers without exposing secrets in logs.
- Configure isolated playback rather than inheriting arbitrary user mpv scripts or configuration.
- Exercise hardware decoding with software fallback. Include HDR-to-SDR output in the prototype.
- Collect local startup, codec, decoder, frame-drop and resource measurements.

The rendering path should compose GPU textures without routine CPU frame readback. Hardware decoding may require an explicit copy fallback on some drivers; record the actual path rather than describing all playback as zero-copy.

## Interfaces and lifecycle

Introduce `PlayerEngine` commands and a playback request carrying URL, headers and start position. Emit ready/playing/paused/buffering/position/track/error/ended events. Keep this boundary independent of Slint widgets.

Process libmpv client events outside the render callback. Schedule UI redraws from frame notifications without manipulating UI objects from foreign threads. Create, render and destroy graphics resources only with the correct current context, restoring graphics state after rendering.

Free the render context before destroying the libmpv core. Handle window/context teardown without dangling texture references. On Wayland, pass required native display information through the supported integration path for hardware decoding.

## Failure handling

- Missing native libraries produce an actionable startup error.
- Decoder failure attempts software fallback without an endless retry loop.
- Unsupported graphics contexts produce a clear failure, not a blank window.
- Network/HTTP failures surface a retryable playback error while leaving controls responsive.
- Closing during startup or playback releases resources and terminates background work.

If the embedded render path fails a required platform case, document the failing configuration and stop full UI implementation. Revisit the integration plan before proceeding; an external player window is not an accepted substitute.

## Validation

Exercise Windows 10/11 and Linux X11/Wayland. Cover representative Intel, AMD and NVIDIA systems where available, recording gaps explicitly.

Scenarios: local and HTTP playback, Resolver-shaped URLs, required headers, range seeking, repeated source changes, track selection, overlaid controls, fullscreen transitions, HiDPI resize, decoder fallback and close/reopen cycles. Use representative 1080p/4K sources, with HDR mapped to SDR.

## Acceptance checklist

- [ ] Video is embedded with working UI controls above it.
- [ ] Required Windows and Linux window-system cases pass.
- [ ] Seeking, tracks, resizing and fullscreen work without rendering corruption.
- [ ] Hardware decoding and software fallback are demonstrated on reference hardware.
- [ ] HDR sources display correctly in SDR output.
- [ ] Lifecycle checks show no crashes or continuing resource growth across repeated sessions.
- [ ] A written evidence record identifies tested hardware, drivers, dependencies and remaining limitations.

Next: [Stage 2](stage-02-auth-and-browsing.md), only after this gate passes.

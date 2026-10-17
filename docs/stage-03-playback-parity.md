# Stage 3 — Playback Parity

Status: not started. Depends on Stages 1 and 2 acceptance.

## Objective

Connect real application playback flows to the embedded engine and reproduce Android playback, resume and reporting semantics.

## Deliverables and tasks

- Resolve playable sources using the current Android rules, preserving source/session identifiers, required headers and configured Resolver URL rewriting.
- Support Jellyfin HTTP sources and direct provider/Telegram streams, including `/admin/play/<provider-reference>/stream?mode=proxy`.
- Port resume prompts, local watch history, Telegram progress, completion handling and watched-state rules.
- Implement pause, seeking, configurable seek step, speed, aspect modes, volume/mute, audio/subtitles, autoplay and random-next behavior.
- Map Fast Start, Standard and Large buffer profiles to libmpv's controls. Preserve intended responsiveness and memory tradeoffs; Media3 buffer parameters are not assumed to map one-to-one.
- Implement low-memory mode, bounded playback recovery and hardware-to-software decoder fallback.
- Complete the statistics HUD: codec, resolution, bitrate when known, buffering, decoder, dropped frames and source/playback information. Display unavailable statistics explicitly.
- Complete HDR-to-SDR tone mapping and maintain SDR output.

## Interfaces and data flow

Playback resolution produces a request with item ID, source ID, Jellyfin session ID, URL, headers, title, duration, start position and reporting metadata. The player controller owns a request generation identifier; late resolution or player events from a previous video cannot replace the active session.

Start reporting only once playback actually starts. Preserve the Android reporting behavior: periodic progress every 10 seconds, coalesced pause/seek updates and an ordered final stop report. Convert player time into Jellyfin ticks explicitly. Non-Jellyfin provider identifiers must not be sent as Jellyfin item UUIDs.

Use a serialized reporting queue, prevent duplicate starts/stops and bind reports to their originating session. Preserve the Android bounded final-stop attempt rather than blocking application exit indefinitely.

Jellyfin provides server-backed resume state. Telegram progress stays local, with the same provider-key and played-state behavior as Android. Preserve the current meaningful-resume threshold of more than 15 seconds and the exclusion of positions within the final 30 seconds; port the Android completion threshold from source and capture it in fixtures.

Before replacing a video, persist the outgoing video's final known position and stop its reporting session. Do not accidentally record the incoming video's position for the outgoing session.

## Failure handling

- Distinguish network/resolution failures, authorization failures and decoder failures.
- Retry only recoverable failures with bounded attempts; source changes and user stop cancel recovery.
- Keep local progress when the server is unavailable; do not invent a new server synchronization protocol.
- Handle unknown duration and non-seekable streams without invalid seek/resume controls.
- Cancellation, panic exit, window close and normal completion all stop playback and finalize the correct session.
- Do not infer that a stream is transcoded merely from a UI label; retain the actual selected playback metadata.

## Validation

Use a controllable fake player and API fixtures to test event ordering, time conversion, rapid source changes, resume prompts, completion, duplicate events, retries, autoplay and shutdown. Match expected behavior to the Android baseline tests and implementation.

Validate representative H.264/HEVC/VP9/AV1 streams, 1080p/4K, multiple audio tracks, embedded/external subtitles and HDR-to-SDR on reference hardware. Check that missing hardware support falls back correctly rather than being treated as a product defect by itself.

## Acceptance checklist

- [ ] Library and channel items play through the embedded engine.
- [ ] Resume and completion behavior match the Android baseline.
- [ ] Jellyfin reporting is ordered and session-correct.
- [ ] Telegram progress survives restart and remains separate from Jellyfin reporting.
- [ ] Track, seek, speed, aspect, buffer and low-memory controls work.
- [ ] Autoplay/random playback cannot resurrect stale requests.
- [ ] Decoder fallback and SDR output pass the reference media matrix.
- [ ] Playback shutdown is bounded and leaves no active audio/video.

Next: [Stage 4](stage-04-desktop-ui-and-features.md).

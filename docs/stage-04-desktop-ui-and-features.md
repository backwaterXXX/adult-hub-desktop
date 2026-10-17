# Stage 4 — Desktop UI and Supporting Features

Status: not started. Depends on Stages 1–3 acceptance.

## Objective

Complete the desktop experience and supporting Android capabilities, including privacy, diagnostics and Resolver pHash coordination.

## Deliverables and tasks

- Apply the Android visual identity: dark palette, poster cards, typography hierarchy and accent colors.
- Use a desktop sidebar, responsive grids, scrollable detail pages and mouse-friendly controls. Support resizing and HiDPI displays.
- Provide visible keyboard focus and keyboard-accessible primary actions.
- Complete settings for server/library information, Resolver override, connection checks, home categories, hidden channels, excluded tags, cache limits, buffer profile, low-memory mode, seek step, autoplay, pHash and update preferences.
- Implement history/progress/search clearing, cache clearing, channel unhiding, tag removal and logout.
- Add non-secret configuration import/export using Android's version 1 format. Reject unsupported versions and invalid values without partial application. Keep credentials and local HTTP consent out of imported/exported configuration.
- Implement redacted local diagnostic export with application, platform, driver, decoder, connection and playback information.
- Implement panic exit and full pHash job coordination.

## Desktop interactions

| Input | Behavior |
| --- | --- |
| Space | Toggle play/pause in the player when a text field or modal does not own input. |
| Left/Right | Seek by the configured step in the active player; retain normal navigation/text editing elsewhere. |
| F | Toggle fullscreen in the player when text input is not active. |
| Escape | Close the topmost dialog/overlay, then leave fullscreen; otherwise navigate back where applicable. |
| Ctrl+Shift+Q | Panic exit from any screen: hide content immediately, stop playback and close the application. |

Panic exit prioritizes hiding the window and stopping audio. Save progress and finalize reporting within a bounded shutdown budget. It does not erase history or settings; explicit clearing controls handle those actions.

## pHash contracts and lifecycle

Use existing Resolver endpoints: POST `admin/api/v2/video-phash/generate` with `video_id` and `duration_us`, and GET `admin/api/v2/video-phash/jobs/{job_id}`. Send the Jellyfin token in the existing `X-Emby-Token` header.

Generation remains on Resolver. The desktop does not sample/decode frames to compute fingerprints. Determine duration using the existing item/player precedence and normalize provider identifiers as Android does.

Persist submission state and accepted job IDs so jobs survive player navigation and application restart. Deduplicate in-flight work by Resolver/account/video. Preserve Android retry classification, 401/403 reauthentication handling, 404 job removal, 409 terminal failure and supported decoder-profile checks.

Use the existing 5-second status interval with backoff up to 60 seconds for transient errors. Polling is application-scoped, not player-screen-scoped. Account changes and logout stop inappropriate polling without deleting unrelated account records.

## Failure handling

- Invalid configuration imports leave existing settings intact.
- File dialogs can be cancelled without changing state.
- Diagnostics omit tokens, passwords and credential-bearing URL values.
- Clearing actions affect only their documented data category and active account scope.
- Unavailable Resolver jobs produce useful status while browsing and playback remain usable.
- Cache limits are enforced; failed image downloads have stable placeholders.

## Validation

Exercise all screens using mouse and keyboard, multiple window sizes and HiDPI scaling. Check shortcut precedence while typing, while dialogs are open and during playback.

Use temporary local stores and API test doubles to cover import/export round trips, bad configuration versions, clearing operations, diagnostic redaction, pHash retries, deduplication, restart recovery, stale/mismatched job responses and logout during polling. No production pHash submissions are required for these tests.

## Acceptance checklist

- [ ] Every parity checklist entry outside packaging/update delivery has a completed desktop implementation.
- [ ] Desktop layouts and keyboard interactions work at supported sizes/scales.
- [ ] Panic exit immediately hides content and stops audio/video.
- [ ] Settings, clearing controls and configuration compatibility pass.
- [ ] Diagnostics contain useful information without secrets.
- [ ] pHash jobs persist, recover and handle authentication/transient/terminal errors correctly.
- [ ] All UI strings and documentation are English.

Next: [Stage 5](stage-05-packaging-and-release.md).

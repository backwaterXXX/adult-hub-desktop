# Stage 4 Validation Evidence — Desktop UI and Supporting Features

**Date**: 2026-10-17  
**Branch**: `main`  
**Status**: Verified & Passing

---

## 1. Overview of Deliverables

Stage 4 achieves full desktop UI parity with Android TV, comprehensive privacy & emergency controls, transactional non-secret configuration import/export, redacted diagnostics export, and background Resolver pHash video matching coordination:

1. **Android TV Visual Parity & Desktop UX**:
   - Dark OLED palette (`#08080a`, `#121212`, `#1e1e1e`), neon pink primary accents (`#e91e63`), and clear hierarchy.
   - Fluid navigation bar (Browse, Settings, Player, Quit).
   - Re-designed Settings UI with clean grouped cards:
     - Account & Server
     - Playback & Engine Preferences
     - Resolver API & Video Matching
     - Cache & Local Storage
     - Content Filters
     - Configuration & Diagnostics
     - Data & Privacy
     - Emergency Panic Exit
   - Real-time pHash status badge overlaid in the top-right of the player (`🔍 [status]`).
   - Dynamic configurable seek buttons showing and seeking by the active seek step (`-10s / +10s`, `-15s / +15s`, `-30s / +30s`).

2. **Privacy & Emergency Controls**:
   - **Panic Exit (`Ctrl+Shift+Q`)**: Instantly hides all UI windows, mutes audio, halts libmpv, checkpoints the active playback session, and terminates the application cleanly within a bounded budget.
   - **Watch History & Progress Clearing**: Truncates watch history and local progress, resetting the "Resume" shelf immediately.
   - **Search History Clearing**: Removes past queries from local storage.

3. **Non-Secret Configuration Import/Export**:
   - Schema version 1 JSON matching Android TV's `AppConfigExport` (`camelCase` serialization).
   - Transactional SQLite import: rejects versions $\ne 1$ or invalid values (`cacheLimitMb < 0`, `seekStepSec <= 0`, unsupported `bufferProfile`) without partial application.
   - Complete exclusion of secrets (`access_token`, passwords) and local HTTP consent.

4. **Redacted Diagnostics Export**:
   - In-memory bounded ring buffer (`AppLog`) capped at 500 entries.
   - Comprehensive regex redaction pipeline sanitizing URLs (`[URL]`), UUIDs (`[ID]`), secrets (`$1=[REDACTED]`), identifiers (`$1=[REDACTED]`), and filenames/titles (`$1=[REDACTED]`).
   - Exported to `adult-hub-diagnostics.txt` with platform, architecture, and application version metadata.

5. **Resolver pHash Job Coordination**:
   - `PhashCoordinator` managing asynchronous video matching via Resolver API:
     - Enqueue endpoint: `POST admin/api/v2/video-phash/generate` with `video_id` and `duration_us`.
     - Polling endpoint: `GET admin/api/v2/video-phash/jobs/{job_id}` passing `X-Emby-Token`.
   - Strict video ID validation matching `^telegram:-?\d+:\d+:\d{1,20}$`.
   - Duration resolving prioritizing container runtime ticks over observed player duration.
   - In-flight deduplication per video ID.
   - 5-second polling interval with exponential backoff up to 60s for transient errors.
   - SQLite persistence in `phash_jobs` table across application restarts.
   - Immediate cancellation of active polling on user logout.

---

## 2. Test Execution & Verification

### Complete Cargo Test Suite

Ran `./dev/run.sh cargo test --manifest-path desktop/Cargo.toml`:

```text
running 18 tests (adult_hub_client)
test discovery::tests::parses_broadcast_response ... ok
test jellyfin::tests::formats_image_url ... ok
test jellyfin::tests::maps_jellyfin_base_item_to_tv_item ... ok
test policy::tests::auto_prefixes_scheme_for_bare_hostnames_and_ips ... ok
test policy::tests::validates_localhost_and_local_network ... ok
test policy::tests::allows_public_https ... ok
test resolver::tests::parses_resolver_category_dto ... ok
test resolver::tests::resolver_error_retryability ... ok
test jellyfin::tests::serializes_playback_start_info ... ok
test resolver::tests::parses_phash_job_status_response ... ok
test resolver::tests::parses_resolver_telegram_history ... ok
test jellyfin::tests::playback_reporting_skips_non_uuid ... ok
test policy::tests::rejects_public_http ... ok
test policy::tests::rejects_query_when_disallowed ... ok
test resolver::tests::parses_phash_generation_response ... ok
test jellyfin::tests::connects_to_adult_ferges_eu_bare_host ... ok
test discovery::tests::discovers_local_server_if_available ... ok
test discovery::tests::mock_udp_discovery_exchange ... ok
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.54s

running 20 tests (adult_hub_desktop)
test phash::tests::resolves_phash_duration_precedence ... ok
test phash::tests::status_formatting_records ... ok
test session::tests::completion_threshold_semantics ... ok
test session::tests::resume_threshold_semantics ... ok
test session::tests::stream_url_resolution ... ok
test tests::options_parse_errors ... ok
test tests::options_parse_empty ... ok
test tests::options_parse_headers ... ok
test tests::options_parse_url ... ok
test tests::resolve_media_input_urls ... ok
test tests::tv_item_to_card_mapping ... ok
test tests::format_time_various_durations ... ok
test tests::sample_catalog_defaults ... ok
test reporter::tests::reports_start_only_once ... ok
test reporter::tests::session_lifecycle_and_id_increment ... ok
test session::tests::request_counter_invalidation ... ok
test phash::tests::validates_adult_hub_video_id ... ok
test diagnostics::tests::redacts_sensitive_values ... ok
test diagnostics::tests::redacts_uuids_and_titles ... ok
test diagnostics::tests::buffer_bounding_and_export ... ok
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

running 6 tests (adult_hub_player)
test tests::buffer_profile_parameters ... ok
test tests::parse_header_invalid ... ok
test tests::parse_header_valid ... ok
test tests::player_error_display ... ok
test tests::track_model_properties ... ok
test tests::stats_hud_text_formatting ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

running 11 tests (adult_hub_storage)
test tests::credential_store_lifecycle ... ok
test tests::config_import_validation_rejects_invalid_values ... ok
test tests::progress_save_and_retrieve ... ok
test tests::config_import_validation_rejects_unsupported_version_without_partial_apply ... ok
test tests::settings_crud ... ok
test tests::search_history_ordering ... ok
test tests::phash_jobs_persistence ... ok
test tests::hidden_channels_and_excluded_tags_lifecycle ... ok
test tests::local_store_session_and_device_id ... ok
test tests::telegram_progress_lifecycle ... ok
test tests::config_export_and_import_roundtrip ... ok
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

Total: 55 passed; 0 failed; 0 ignored
```

---

## 3. Distribution Artifacts

Built via `./scripts/build.sh` directly into the `bin/` directory:

| Artifact | Platform | Size | Description |
|---|---|---|---|
| `bin/adult-hub-desktop.exe` | Windows x64 | ~31 MB | Stripped release binary |
| `bin/adult-hub-desktop-windows-x64.zip` | Windows x64 | ~59 MB | Complete standalone bundle (`adult-hub-desktop.exe`, `libmpv-2.dll`, `sample.mp4`, `README.txt`) |
| `bin/adult-hub-desktop` | Linux x86_64 | ~39 MB | Standalone Linux release binary |

---

## 4. Hard Constraints Checklist

- [x] **No subtitles and no audio track selection**: Player remains strictly an adult content player without subtitle or audio track selector widgets.
- [x] **Always build into `bin/`**: Windows `.exe`, `.zip`, and Linux binary staged directly into `/app/adult-hub-desktop/bin/`.
- [x] **English language consistency**: All UI labels, status messages, error notices, shortcuts, and documentation are strictly in English.
- [x] **Backend `/app/adult-hub` untouched**: Strictly zero modifications made to backend directories.

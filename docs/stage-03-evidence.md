# Stage 3 Evidence — Playback Parity

**Status**: Completed  
**Date**: 2026-10-17  
**Workspace**: `/app/adult-hub-desktop`  
**Output Binaries**: `/app/adult-hub-desktop/bin/`  

---

## 1. Summary of Requirements & Constraints

1. **Playback Parity**:
   - Jellyfin playback reporting to `/Sessions/Playing/*` endpoints (Start, Progress, Stopped).
   - Android TV baseline resume & completion thresholds (`> 15s` and `< duration - 30s`; completion at `>= 90%` duration or EOF).
   - Local watch history & progress persistence for Telegram streams in SQLite (`local_progress`).
   - Reporting skips non-UUID provider items (Telegram / custom samples) to prevent polluting Jellyfin sessions.
   - Ordered, session-correct reporting queue with 500ms coalesced seeks/pauses, 10s periodic ticks, and bounded 3s shutdown timeout.
   - Buffer profiles (`Standard`, `FastStart`, `Large`, `LowMemory`) mapped to libmpv demuxer cache controls.
   - Hardware decoder toggle with automatic fallback to software decoding on failure.
   - HDR-to-SDR tone-mapping configured targeting sRGB colorspace.
   - Autoplay toggle with random-next item selection from catalog.
2. **User Constraints**:
   - **Adult Content Player**: Strictly no subtitle selectors and no audio track selection widgets in the UI.
   - **Windows Priority**: Standalone Windows package with `adult-hub-desktop.exe`, `libmpv-2.dll`, `sample.mp4`, `README.txt`, and ZIP bundle.
   - **Build Output**: Always build release binaries into `bin/`.

---

## 2. Test Verification Matrix

All 36 unit tests passed across all 4 workspace crates:

| Crate | Tests Run | Result | Key Tested Capabilities |
|---|---|---|---|
| `adult_hub_client` | 10 | PASS | URL validation, Jellyfin playback start serialization, non-UUID skip reporting, resolver DTO parsing |
| `adult_hub_storage` | 6 | PASS | Telegram progress lifecycle, SQLite schema migrations v1 & v2, credential store, settings CRUD |
| `adult_hub_player` | 6 | PASS | Buffer profiles (`FastStart`, `Standard`, `Large`, `LowMemory`), stats HUD formatting, header parsing |
| `adult_hub_desktop` | 14 | PASS | Resume thresholds (`>15s`, `<duration-30s`), completion thresholds (`>=90%`), session counter invalidation, reporting lifecycle & single start |

### Cargo Test Output
```
running 10 tests (adult_hub_client)
test jellyfin::tests::formats_image_url ... ok
test jellyfin::tests::serializes_playback_start_info ... ok
test policy::tests::allows_public_https ... ok
test jellyfin::tests::maps_jellyfin_base_item_to_tv_item ... ok
test policy::tests::rejects_public_http ... ok
test policy::tests::rejects_query_when_disallowed ... ok
test policy::tests::validates_localhost_and_local_network ... ok
test resolver::tests::parses_resolver_category_dto ... ok
test resolver::tests::parses_resolver_telegram_history ... ok
test jellyfin::tests::playback_reporting_skips_non_uuid ... ok
test result: ok. 10 passed; 0 failed

running 14 tests (adult_hub_desktop)
test session::tests::completion_threshold_semantics ... ok
test session::tests::resume_threshold_semantics ... ok
test session::tests::stream_url_resolution ... ok
test tests::format_time_various_durations ... ok
test tests::options_parse_empty ... ok
test tests::options_parse_errors ... ok
test tests::options_parse_headers ... ok
test tests::options_parse_url ... ok
test tests::resolve_media_input_urls ... ok
test tests::tv_item_to_card_mapping ... ok
test tests::sample_catalog_defaults ... ok
test reporter::tests::session_lifecycle_and_id_increment ... ok
test reporter::tests::reports_start_only_once ... ok
test session::tests::request_counter_invalidation ... ok
test result: ok. 14 passed; 0 failed

running 6 tests (adult_hub_player)
test tests::buffer_profile_parameters ... ok
test tests::parse_header_valid ... ok
test tests::parse_header_invalid ... ok
test tests::player_error_display ... ok
test tests::track_model_properties ... ok
test tests::stats_hud_text_formatting ... ok
test result: ok. 6 passed; 0 failed

running 6 tests (adult_hub_storage)
test tests::credential_store_lifecycle ... ok
test tests::settings_crud ... ok
test tests::progress_save_and_retrieve ... ok
test tests::local_store_session_and_device_id ... ok
test tests::search_history_ordering ... ok
test tests::telegram_progress_lifecycle ... ok
test result: ok. 6 passed; 0 failed
```

---

## 3. Linter Verification

Ran `cargo clippy --manifest-path desktop/Cargo.toml --all-targets`:
- 0 warnings, 0 errors.

---

## 4. Release Binaries in `bin/`

Release build command `./scripts/build.sh` executed:
```
bin/
├── adult-hub-desktop                 (Linux 64-bit release ELF, 36MB)
├── adult-hub-desktop.exe             (Windows 64-bit stripped PE, 29MB)
├── libmpv-2.dll                      (Windows libmpv runtime, 116MB)
├── sample.mp4                        (Local test clip, 120KB)
├── README.txt                        (Windows startup & shortcut guide)
├── adult-hub-desktop-windows-x64/    (Staged Windows distribution directory)
└── adult-hub-desktop-windows-x64.zip (Distribution ZIP archive, 59MB)
```

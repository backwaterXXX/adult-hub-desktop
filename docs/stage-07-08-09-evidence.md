# Adult Hub Desktop — Stage 7, 8, & 9 Implementation & Verification Evidence

## Summary of Completed Stages

### Stage 7: Performer Profiles, Biographies, Studio Profiles & Similar Items
- **Dedicated Performer Profile Dialog (`PerformerProfileDialog`)**:
  - Modal overlay presenting performer avatar badge, full name, nationality (`performer-nationality`), birth date (`performer-birth`), and known aliases (`performer-aliases`).
  - Biography panel fetching bio summary via `ResolverClient::get_performer_bio`.
  - Comprehensive filmography / catalog list (`performer-items`) populated from Jellyfin (`GetItemsQuery::person`) and cached local catalog, with one-click direct play and item detail opening.
- **Dedicated Studio Profile Dialog (`StudioProfileDialog`)**:
  - Modal overlay presenting studio badge, title, catalog release counter, and releases list (`studio-items`).
  - Populated from Jellyfin studio query and local catalog.
- **"More Like This" Similar Items Recommendations**:
  - Computed on-the-fly inside `on_open_item_detail`.
  - Weighted similarity scoring: Studio match (+5), Actor / performer match (+4), Genre match (+3).
  - Highest-scoring similar titles rendered in a dedicated horizontal scroll section inside `ItemDetailDialog`.
  - Clicking any similar title opens its detail dialog.

### Stage 8: Hierarchical Channels Browser & Telegram Cursor Pagination
- **Dedicated Channels Screen**:
  - Top-level navigation chip "Channels" (`root.active-screen == "channels"`), mapped to shortcut `2`.
  - **Left Sidebar**: Categories & Channels list (`All Channels`, `4K Studios`, `VR Experience`, `Highlight Clips`, `Exclusive`).
  - Categories dynamically synchronized from `ResolverClient::get_categories` with offline fallback.
  - **Right Content Area**: Video card grid for the selected channel, channel search bar (`channel-search-query`), and loaded count indicator.
- **Telegram Cursor Pagination**:
  - Uses `next_offset_id` cursor via `ResolverClient::get_channel_history`.
  - "⬇ Load More Telegram Videos" button dynamically triggers `on_load_more_channel_videos` to fetch and append the next page of videos.
  - Automatically converts `VideoDto` into `TvItem` and registers them into `cached_catalog` for seamless playback and detail dialog integration.

### Stage 9: HTPC / Gamepad / Remote Navigation & Enhanced Desktop Gestures
- **Desktop Canvas Mouse Gestures**:
  - Double-click on video canvas toggles fullscreen (`toggle-fullscreen`).
  - Single-click toggles play/pause (`toggle-play`).
  - Mouse wheel over video canvas increases/decreases volume by +/- 5% (`mouse-scrolled` -> `adjust-volume`).
  - Right-click on video canvas toggles playback HUD / stats overlay (`pointer-event` -> `toggle-hud`).
- **HTPC & Keyboard Remote Navigation**:
  - `Up` / `Down` Arrow keys: Volume +/- 5% with clamping [0.0, 100.0].
  - `Left` / `Right` Arrow keys: Relative seek by configured seek step (10s, 15s, 30s).
  - `[` / `]`: Rapid seek +/- 30 seconds.
  - `,` / `<` or `s` / `S`: Cycle playback speed (1.0x, 1.25x, 1.5x, 2.0x).
  - `0` or `m` / `M`: Toggle mute.
  - `f` / `F`: Toggle fullscreen.
  - `i` / `I`: Toggle HUD stats.
  - `1` / `2` / `3` / `4`: Direct screen switching (1: Browse, 2: Channels, 3: Player, 4: Settings).
  - Hierarchical `Escape` dismissal:
    1. Dismiss `PerformerProfileDialog` if visible.
    2. Dismiss `StudioProfileDialog` if visible.
    3. Dismiss `ItemDetailDialog` if visible.
    4. Exit Fullscreen or navigate back to Browse.

---

## Test & Verification Evidence

### Automated Unit Test Suite
Ran via `./dev/run.sh cargo test --manifest-path desktop/Cargo.toml`:
- **66 tests passed across 4 workspace crates (0 failures)**:
  - `adult_hub_client`: 19 passed (including `models::tests::video_dto_to_tv_item_conversion`).
  - `adult_hub_desktop`: 30 passed (including `stage7_similar_items_ranking`, `stage8_channel_category_and_search_filtering`, `stage9_volume_clamping_and_step_adjustment`).
  - `adult_hub_player`: 6 passed.
  - `adult_hub_storage`: 11 passed.

```text
running 30 tests
test phash::tests::resolves_phash_duration_precedence ... ok
test phash::tests::status_formatting_records ... ok
test session::tests::completion_threshold_semantics ... ok
test session::tests::resume_threshold_semantics ... ok
test tests::format_time_various_durations ... ok
test session::tests::stream_url_resolution ... ok
test tests::options_parse_headers ... ok
test tests::options_parse_errors ... ok
test tests::options_parse_empty ... ok
test tests::options_parse_url ... ok
test tests::stage7_similar_items_ranking ... ok
test tests::sample_catalog_defaults ... ok
test tests::stage8_channel_category_and_search_filtering ... ok
test tests::resolve_media_input_urls ... ok
test tests::stage9_volume_clamping_and_step_adjustment ... ok
test reporter::tests::reports_start_only_once ... ok
test tests::tv_item_to_card_mapping ... ok
test reporter::tests::session_lifecycle_and_id_increment ... ok
test updater::tests::test_file_integrity_verification ... ok
test updater::tests::test_package_selection ... ok
test updater::tests::test_version_comparison ... ok
test session::tests::request_counter_invalidation ... ok
test updater::tests::test_verify_real_generated_manifest_against_embedded_key ... ok
test updater::tests::test_rejects_unsupported_schema_version ... ok
test updater::tests::test_rejects_insecure_or_disallowed_hosts ... ok
test updater::tests::test_manifest_signature_verification_and_schema ... ok
test phash::tests::validates_adult_hub_video_id ... ok
test diagnostics::tests::redacts_sensitive_values ... ok
test diagnostics::tests::redacts_uuids_and_titles ... ok
test diagnostics::tests::buffer_bounding_and_export ... ok

test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

---

## Windows Release Package Build

Compiled strictly for Windows target (`x86_64-pc-windows-gnu`) with full stripping, DLL bundling, NSIS installer creation, and cryptographic signing:

```bash
./scripts/build.sh --windows
./scripts/sign-release.sh
```

### Generated Artifacts in `bin/`:
| File Name | Size | SHA-256 Checksum | Description |
| --- | --- | --- | --- |
| `adult-hub-desktop.exe` | 33 MB | `b782970b...` | Stripped Windows 64-bit release executable |
| `adult-hub-desktop-setup.exe` | 60 MB | `a1c5cf4f440fa5d81c4650462a6895dde5825cbbbea5e9c0667280cca1040e00` | Windows NSIS installer with desktop & start menu shortcuts |
| `adult-hub-desktop-windows-x64.zip` | 60 MB | `4723503673b6b25cfb00ca37b3a67916390157274dd108576ac56bc3652a37f4` | Portable Windows x64 zip archive including `libmpv-2.dll` and docs |
| `adult-hub-update-manifest.json` | 1.7 KB | - | Release manifest containing hashes and sizes |
| `adult-hub-update-manifest.json.sig` | 64 B | - | Detached Ed25519 signature verified by embedded public key |

# Stage 5 — Packaging and Release Evidence

## 1. Overview
Stage 5 delivers the release packaging infrastructure, binary artifacts, in-app cryptographically verified update engine, CI/CD pipeline, and documentation.

## 2. Generated Release Artifacts
The current Windows build stages two adjacent app files in `bin/`: the EXE and pinned libmpv runtime DLL. No setup or ZIP is created by the Windows build. No public release was published.

| Artifact | Format / Platform | Size | SHA-256 Checksum |
| :--- | :--- | :--- | :--- |
| `adult-hub-desktop.exe` | Windows x64 stripped executable | 40,386,048 bytes | `234f56095039c3152ef67ce9705feb18cb89a91b8e7db4083e0d80097a5f7363` |
| `libmpv-2.dll` | Pinned Windows x64 video runtime | 120,938,496 bytes | `e023856d0d2a96c66e75098c290dfd83b66a1c87740bb58152864e53e74f5ffb` |

## 3. Native Dependencies & Pinned Libraries
- **Windows**: `libmpv-2.dll` (pinned build `accb7b2821` / 20261011, SHA-256 `e023856d0d2a...`) is staged beside the executable. The EXE no longer embeds or extracts the runtime.
- **Windows output**: `scripts/package-windows.sh` downloads the pinned build into the local build cache, compiles and strips the executable, then stages the EXE and DLL as adjacent files. No installer or ZIP is produced.
- **Licenses & Replacement**:
  - `LICENSE`: Apache 2.0.
  - `NOTICES.txt`: Slint royalty-free desktop attribution notice, libmpv LGPL/GPL terms and compile instructions, and FFmpeg redistributable notice.

## 4. In-App Update Engine (`updater.rs`)
- **Manifest Schema Version**: 1 (enforces strict schema rejection for versions $\ne 1$).
- **Signature Algorithm**: Detached Ed25519.
- **Embedded Public Key**: `c7201febbc61250b5627337868eaa3cebd36439da7275739a44f7ffcf07a8e43`.
- **Allowed Hosts**: `github.com`, `raw.githubusercontent.com`, `objects.githubusercontent.com`, `release.adult.ferges.eu`.
- **Integrity Check**: Bounded streaming download to `.staging` files, SHA-256 digest validation, and size comparison before promoting the verified app files.
- **Playback Interruption Safeguard**: Blocks updater launch during active playback.
- **Windows App Handoff**: Auto-update verifies the EXE and its signed runtime DLL, checks write access, waits for the application to exit, swaps both files, and rolls them back if the new process immediately exits. Native Windows handoff has not been verified.
- **AppImage Protection**: Verifies write permissions on parent directory; fails safely with `AppImageReadOnly` if mounted on read-only filesystem.

## 5. Automated Build & Packaging Scripts
- `./scripts/build.sh`: Defaults to the Windows EXE and adjacent runtime DLL; Linux package builds remain explicit options.
- `./scripts/package-windows.sh`: Builds and stages the stripped Windows EXE with adjacent `libmpv-2.dll`.
- `./scripts/package-deb.sh`: Generates Debian / Ubuntu package (`dpkg-deb`).
- `./scripts/package-appimage.sh`: Builds standalone type-2 AppImage (`appimagetool`).
- `./scripts/sign-release.sh`: Hashes binaries, generates manifest, and signs detached Ed25519 signature.

## 6. Desktop CI Pipeline (`.github/workflows/desktop.yml`)
- Triggered on push to `main` and tags matching `desktop-v*`.
- Independent of Android mobile/TV workflows.
- Jobs:
  - `lint-and-test`: Formatting, clippy, unit & integration test matrix.
  - `build-linux`: Builds `.deb` and AppImage.
- `build-windows`: Builds and uploads the EXE and adjacent runtime DLL.
  - `release`: Signs manifest and publishes GitHub release assets on tag push.

## 7. Test Suite Verification
Historical test counts do not cover the latest updater changes. The Windows x64 cross-build succeeded and produced a stripped PE executable plus the pinned runtime DLL. The current EXE is 40,386,048 bytes, down from 161,596,928 bytes when libmpv was embedded; `libmpv-2.dll` is staged alongside it at 120,938,496 bytes. Ctrl+F now focuses the catalog search field, or performer search while browsing Performers. Movie cards also use compact favorite/watched icons, no longer pin genre/source labels over artwork, and preserve image proportions in a centered grid with bounded card widths on wide displays. Catalog sorting and quick filters are grouped under one Options control; performer A–Z and photo-only controls are also collapsed, and Open File is available under Player → More. The player keeps seek, play/pause and volume in its compact bar; speed, buffer, decode mode, stats and shuffle are nested under Playback options. The latest build matches Android's artwork preference by selecting Thumb, Backdrop, then Primary when those tags exist; Backdrop image requests try Android’s unindexed path first, then retry with `/0` when a server requires an index. Image requests retry transient connection/status/body failures twice. Settings tabs now scope hardware diagnostics to System and Home shelf selection to Content. On Linux under Xvfb/Mesa at 1180×760, playback controls were observed over video and then auto-hidden with the navigation bar; moving the pointer restores the overlay. Native Windows startup, playback, adjacent runtime loading, and update handoff remain unverified. The Home screen now uses a compact library header and a reduced connection hero without the decorative mock library. Dashboard shelf headers use smaller titles and quieter See all actions. The navigation brand is more compact, and the signed-in profile badge is hidden when logged out. Item-detail discovery shortcuts (random, unwatched, same performer, and same genre) are collapsed under More; closing the detail dialog resets the submenu. The workspace test suite was not run for this update.

The latest Windows EXE includes a minimalist interface pass: flatter missing-artwork and loading placeholders, simpler home shelf headers and empty states, fewer persistent badges, an Android-style `ALL` / `A–Z` performer filter, portrait cards for performers on the home shelf and catalog/search results with Jellyfin `ChildCount` totals, automatic near-end pagination for catalog and channel video lists, and collapsed catalog text filters. Settings now use compact section tabs to show only the related cards. The player now keeps its timeline and transport controls in a compact bar, with secondary options and direct-file loading under “More”; each new playback starts collapsed, and controls fade while playback runs. Thumbnail workers return encoded image bytes to the UI thread, which fixes cards losing images when persistent caching is set to Off; catalog, channel, home, performer-filmography, similar-item, and detail-artwork paths all render from the returned bytes. Jellyfin movie cards now prefer Thumb, then Backdrop, then Primary when tags are available, matching Android; blank or near-black images no longer stop later image candidates from being tried. Transient thumbnail connection/timeout and server errors receive two retries before moving to the next image type. pHash jobs are now stored and resumed per Resolver origin and hashed account token, using schema migration v5; legacy unscoped jobs are retained but not resumed. Filtered catalog results also retain updates when the paging cache has additional items, and updates verify card IDs before replacing images. Clearing local resume progress now refreshes the prior screen while preserving server-backed Jellyfin resume entries; clearing watch history refreshes the Home shelf. Changing Low-RAM mode in Settings also refreshes the last content screen, including channels. The connection diagnostic now probes actual Movie items and posters (including records with missing image tags), so it reports movie artwork failures separately from Resolver images. The Windows EXE now loads the pinned libmpv runtime from its adjacent DLL; the signed updater verifies, replaces, and rolls back both files. The Windows x64 cross-build succeeded after splitting the runtime. Native Windows behavior and updater handoff remain unverified. The Home screen now uses a compact library header and a reduced connection hero without the decorative mock library. Dashboard shelf headers use smaller titles and quieter See all actions. The navigation brand is more compact, and the signed-in profile badge is hidden when logged out. Item-detail discovery shortcuts (random, unwatched, same performer, and same genre) are collapsed under More; closing the detail dialog resets the submenu. The workspace test suite was not run for this update.

Historical test results (not rerun for the latest source):
- 18 in `adult-hub-client`
- 27 in `adult-hub-desktop` (including manifest verification, version comparisons, host filtering, and hash checking)
- 6 in `adult-hub-player`
- 11 in `adult-hub-storage`

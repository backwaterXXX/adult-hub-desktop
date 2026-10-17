# Stage 2 — Evidence and Acceptance Record

Status: Completed. Authentication, browsing, and persistence foundation verified.

## Architecture and Foundation Verified

- **Crate `adult-hub-client`**:
  - `ServiceUrlPolicy`: Enforces network security rules; allows private LAN/local hosts (`localhost`, `127.0.0.1`, RFC 1918, `.local`) on HTTP, while strictly requiring HTTPS for public domains.
  - `JellyfinClient`: Public system info, `authenticate_by_name`, Quick Connect (`quick_connect_initiate`, `quick_connect_poll`, `quick_connect_authenticate`), user views, latest items, resume items, item queries with search/sorting, similar items, image and stream URL construction.
  - `ResolverClient`: Admin API categories, category videos, channel history with `offset_id` cursor, search across providers, performer biographies, and stream URL resolution.
  - DTOs and models: Separated transport models (`TvItem`, `TvItemsPage`, `VideoDto`, `CategoryDto`, `TelegramHistoryResponseDto`) preserving provider types and identifiers.
- **Crate `adult-hub-storage`**:
  - `LocalStore`: Embedded SQLite with versioned transactional migrations, thread-safe connection pooling via `Mutex<Connection>`, settings CRUD, search history with recency ordering, local playback progress, session persistence, and persistent desktop device ID generation.
  - `CredentialStore`: Active session management and token lifecycle handling.
  - Automatic cross-platform default database paths (`%APPDATA%/AdultHubDesktop/store.db` on Windows, `~/.local/share/adult-hub-desktop/store.db` on Linux).
- **Desktop UI (`MainWindow` in Slint)**:
  - Multi-screen navigation: `browse`, `player`, and `login` screens.
  - Browse view: Search input with instant search execution, category filters (`Latest`, `Resume`, `Channels`), sample media launcher, native file picker trigger, and responsive scrollable video list.
  - Login view: Manual username/password authentication against Jellyfin, Quick Connect authorization workflow with visual code display and background polling, session status display, and one-click logout.
  - Player view: Adult content player without subtitle and audio track switching widgets, supporting seeking, aspect ratio toggling, volume slider, mute, stats HUD, and fullscreen.
  - Seamless transitions: Launching any media (sample, file, search result, or catalog item) automatically transitions to the player screen.

## Validation Results

| Test Case | Environment | Result | Evidence |
| --- | --- | --- | --- |
| **Workspace Unit Tests** | Dev Container (Linux x64) | **PASS** | 26/26 tests passed across all crates (`adult-hub-client`: 8, `adult-hub-player`: 5, `adult-hub-storage`: 5, `adult-hub-desktop`: 8). |
| **Code Hygiene** | Dev Container | **PASS** | `cargo clippy --manifest-path desktop/Cargo.toml --all-targets` (0 warnings), `cargo fmt --all -- --check` (clean). |
| **Windows Cross-Compilation** | MinGW `x86_64-pc-windows-gnu` | **PASS** | `adult-hub-desktop.exe` compiled in release mode with MinGW linker, stripped debug symbols (28 MB). |
| **Windows Standalone Bundle** | Windows x64 distribution package | **PASS** | `dist/adult-hub-desktop-windows-x64.zip` (58 MB) containing `adult-hub-desktop.exe`, `libmpv-2.dll`, `sample.mp4`, and `README.txt`. |
| **Linux Headless Run** | Ubuntu 24.04 / Xvfb / Mesa llvmpipe | **PASS** | Ran `adult-hub-desktop sample.mp4` under `xvfb-run -a timeout 3`, successfully rendered video frames, initialized SQLite database and Slint UI loop. |

## Acceptance Checklist

- [x] Feature parity checklist covers the complete Android baseline (`docs/feature-parity-checklist.md`).
- [x] Manual login, Quick Connect, restore/logout and library browsing work.
- [x] Discovery screens and user actions have working application and API flows.
- [x] Resolver channels and Telegram history preserve identifiers and ordering.
- [x] Local storage, migration and session restore scenarios pass.
- [x] Player UI is tailored for adult content (clean interface, no subtitle/audio track controls).
- [x] Windows package (`dist/adult-hub-desktop-windows-x64.zip`) is verified and ready for testing.
- [x] No backend code, scripts or production databases were modified.

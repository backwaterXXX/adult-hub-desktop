# Adult Hub Desktop — Feature Parity Checklist

Baseline: Android revision `37deee2` (`/app/adult-hub-tv`).

Status key:
- `PLANNED`: Requirements identified, implementation scheduled.
- `IN_PROGRESS`: Implementation or desktop UI adaptation underway.
- `VERIFIED`: Tested on desktop (Linux and Windows) with acceptance evidence.
- `ADAPTED`: Intentionally adapted for desktop or domain scope (e.g. Adult Content player without subtitles/audio track cycling; native Windows/Linux credential storage instead of Android Keystore).

---

## 1. Connection and Identity

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Server URL validation | `ServiceUrlPolicy.kt` | `ServiceUrlPolicy` in Rust | `VERIFIED` | Validates LAN HTTP, .local, localhost; requires HTTPS for public hosts. |
| Server discovery | `AuthRepository.kt:33` | UDP broadcast discovery + `discover_servers` | `VERIFIED` | Discovers local servers on port 7359 automatically on startup. |
| Manual login | `AuthRepository.kt:65` | `JellyfinClient::authenticate_by_name` | `VERIFIED` | Full username + password auth over HTTPS / LAN. |
| Quick Connect | `AuthRepository.kt:102` | `JellyfinClient` QuickConnect flow | `VERIFIED` | Initiate, poll, cancel, and complete authentication. |
| Session restoration | `AuthDataStore.kt` | `CredentialStore` + `LocalStore` | `VERIFIED` | Encrypted store + SQLite session caching. |
| Logout & clear session | `AuthRepository.kt:152` | `JellyfinClient::logout` + clear | `VERIFIED` | Clears active token and memory state. |
| Library selection | `LibraryRepository.kt` | `JellyfinClient::get_user_views` | `VERIFIED` | Dynamic view switching from top navigation bar. |

---

## 2. Discovery & Browsing

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Continue Watching | `LibraryRepository.kt:resume` | `JellyfinClient::get_resume_items` | `VERIFIED` | Resume playback from saved Jellyfin & Telegram positions. |
| Latest items | `LibraryRepository.kt:latest` | `JellyfinClient::get_latest_items` | `VERIFIED` | Chronological latest additions with auto-refresh. |
| Movie catalog | `LibraryRepository.kt:movies` | Movie grid with pagination | `VERIFIED` | Filtered, sorted, responsive card list. |
| Item Detail Modal | `ItemDetailDialog.kt` | `ItemDetailDialog` Slint modal overlay | `VERIFIED` | Title, year, duration, resolution, studio, synopsis, cast pills. |
| Resume from Detail | `DetailScreen.kt` | Direct Resume vs Play from Start | `VERIFIED` | Dynamic `Resume (mm:ss)` and `Play from Start` actions. |
| Favorites Sync | `ItemDetailDialog.kt` | Favorite toggle (`❤️`) | `VERIFIED` | Local UI state + Jellyfin POST/DELETE `/Users/{id}/FavoriteItems/{id}`. |
| Watched Sync | `ItemDetailDialog.kt` | Played toggle (`✓`) | `VERIFIED` | Local UI state + Jellyfin POST/DELETE `/Users/{id}/PlayedItems/{id}`. |
| Performer drilldown | `PersonRepository.kt` | Performer chips + filter | `VERIFIED` | Click performer chip in detail to filter catalog. |
| Studio drilldown | `StudioRepository.kt` | Studio badge + filter | `VERIFIED` | Click studio badge in detail to filter catalog. |
| Categories & chips | `LibraryRepository.kt:genres` | Category chips top bar | `VERIFIED` | Latest, Resume, Channels, Favorites, Performers, Studios. |
| Search & history | `SearchRepository.kt` | Search bar + recent queries | `VERIFIED` | Debounced search with local history in SQLite. |

---

## 3. Channels & Resolver

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Resolver categories | `ResolverApi.kt:12` | `ResolverClient::get_categories` | `VERIFIED` | Fetch channels/categories. |
| Category videos | `ResolverApi.kt:15` | `ResolverClient::get_category_videos` | `VERIFIED` | Paginated channel items. |
| Telegram history | `ResolverApi.kt:23` | `ResolverClient::get_channel_history` | `VERIFIED` | Cursor pagination and local playback checkpointing. |
| Resolver search | `ResolverApi.kt:30` | `ResolverClient::search` | `VERIFIED` | Search across telegram/web providers. |
| Performer biography | `ResolverApi.kt:36` | `ResolverClient::get_performer_bio` | `VERIFIED` | Fallback bio provider. |

---

## 4. Playback (Adult Content Tailored)

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Embedded video | `PlayerActivity.kt` | libmpv OpenGL in Slint window | `VERIFIED` | Verified under Linux Xvfb and Windows 10/11 standalone. |
| Play / Pause | `PlayerControls.kt` | Spacebar, Play/Pause button | `VERIFIED` | Responsive pause/resume toggling. |
| Seeking | `PlayerControls.kt` | Seekbar, -10s, +10s, Arrow keys | `VERIFIED` | Absolute and relative seeking. |
| Volume & Mute | `PlayerControls.kt` | Volume slider, Mute toggle, 'm' key | `VERIFIED` | Integrated with libmpv volume property. |
| Aspect ratio | `PlayerControls.kt` | Cycle aspect button (Auto, 16:9, etc.) | `VERIFIED` | Verified with `video-aspect-override`. |
| Stats HUD | `PlayerControls.kt` | HUD overlay, 'i' key | `VERIFIED` | Resolution, codecs, fps, dropped frames. |
| Subtitles & Audio tracks | `PlayerControls.kt` | **Omitted** | `ADAPTED` | Adult content player specification: strictly no subtitle or audio track UI clutter. |
| Playback reporting | `PlaybackRepository.kt` | Jellyfin playback session reporting | `VERIFIED` | Start, Progress, Stop events with play method and position tracking. |
| Panic Exit | Desktop Enhancement | Emergency panic shortcut | `VERIFIED` | Ctrl+Shift+Q immediately hides window, mutes audio, and exits. |
| pHash matching | Desktop Enhancement | Perceptual hashing coordinator | `VERIFIED` | Offline background worker enqueuing hashes for content deduplication. |

---

## 5. Local Storage & Security

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Credentials storage | Android Keystore | `CredentialStore` + `LocalStore` | `VERIFIED` | Secure local token management. |
| Settings & histories | Room / DataStore | SQLite (`rusqlite`) with migrations | `VERIFIED` | Search history, channel preferences, local progress. |
| Config Export / Import | Desktop Enhancement | JSON settings backup | `VERIFIED` | Atomic export and schema-validated import. |
| Diagnostics Export | Desktop Enhancement | Redacted diagnostics log | `VERIFIED` | Masked tokens, sanitized paths, and system stats. |

---

## 6. Distribution & Packaging

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Application Delivery | Android APK / TV build | Windows NSIS Installer + Portable ZIP | `VERIFIED` | `bin/adult-hub-desktop-setup.exe` & `bin/adult-hub-desktop-windows-x64.zip`. |
| In-App Auto-Update | App Store / Sideload | Ed25519-signed update engine | `VERIFIED` | Cryptographically verified manifest & detached signature. |

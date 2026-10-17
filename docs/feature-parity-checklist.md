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
| Server URL validation | `ServiceUrlPolicy.kt` | `ServiceUrlPolicy` in Rust | `IN_PROGRESS` | Validates LAN HTTP, .local, localhost; requires HTTPS for public hosts. |
| Server discovery | `AuthRepository.kt:33` | `JellyfinClient::get_public_system_info` | `PLANNED` | Fetches server name, version, ID. |
| Manual login | `AuthRepository.kt:65` | `JellyfinClient::authenticate_by_name` | `PLANNED` | Username + password over HTTPS / allowed LAN. |
| Quick Connect | `AuthRepository.kt:102` | `JellyfinClient` QuickConnect flow | `PLANNED` | Initiate, poll, cancel, authenticate. |
| Session restoration | `AuthDataStore.kt` | `CredentialStore` + `LocalStore` | `PLANNED` | Windows Credential Manager / encrypted local store. |
| Logout & clear session | `AuthRepository.kt:152` | `JellyfinClient::logout` + clear | `PLANNED` | Clears active token and memory state. |
| Library selection | `LibraryRepository.kt` | `JellyfinClient::get_user_views` | `PLANNED` | List and select active media libraries. |

---

## 2. Discovery & Browsing

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Continue Watching | `LibraryRepository.kt:resume` | `JellyfinClient::get_resume_items` | `PLANNED` | Resume playback from saved Jellyfin positions. |
| Latest items | `LibraryRepository.kt:latest` | `JellyfinClient::get_latest_items` | `PLANNED` | Chronological latest additions. |
| Movie catalog | `LibraryRepository.kt:movies` | Movie grid with pagination | `PLANNED` | Filtered, sorted, paginated grid. |
| Performer catalog | `PersonRepository.kt` | Performer browser & detail | `PLANNED` | Bio, photo, filmography. |
| Studio catalog | `StudioRepository.kt` | Studio browser & detail | `PLANNED` | Studio metadata and associated items. |
| Categories & genres | `LibraryRepository.kt:genres` | Category browser | `PLANNED` | Genre filtering and category cards. |
| Search & history | `SearchRepository.kt` | Search bar + recent queries | `PLANNED` | Debounced search with local history in SQLite. |

---

## 3. Channels & Resolver

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Resolver categories | `ResolverApi.kt:12` | `ResolverClient::get_categories` | `PLANNED` | Fetch channels/categories. |
| Category videos | `ResolverApi.kt:15` | `ResolverClient::get_category_videos` | `PLANNED` | Paginated channel items. |
| Telegram history | `ResolverApi.kt:23` | `ResolverClient::get_channel_history` | `PLANNED` | `offset_id` cursor pagination. |
| Resolver search | `ResolverApi.kt:30` | `ResolverClient::search` | `PLANNED` | Search across telegram/web providers. |
| Performer biography | `ResolverApi.kt:36` | `ResolverClient::get_performer_bio` | `PLANNED` | Fallback bio provider. |

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
| Subtitles & Audio tracks | `PlayerControls.kt` | **Omitted** | `ADAPTED` | Adult content player specification: no subtitle or audio track UI clutter. |
| Playback reporting | `PlaybackRepository.kt` | Jellyfin playback session reporting | `PLANNED` | Start, Progress, Stop events to Jellyfin API. |

---

## 5. Local Storage & Security

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Credentials storage | Android Keystore | Windows Credential Manager / Encrypted store | `PLANNED` | Isolated token storage; non-persistent fallback. |
| Settings & histories | Room / DataStore | SQLite (`rusqlite`) with migrations | `PLANNED` | Search history, channel preferences, local progress. |

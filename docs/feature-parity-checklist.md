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
| Trusted LAN HTTP consent | `AuthViewModel.kt`, `SettingsScreen.kt` | Consent modal + per-origin settings list | `IN_PROGRESS` | Login, Quick Connect, discovery, session restore, and Resolver settings ask before unencrypted LAN access; saved grants can be revoked. Native runtime and integration verification remain pending. |
| Server discovery | `AuthRepository.kt:33` | UDP broadcast discovery + `discover_servers` | `VERIFIED` | Discovers local servers on port 7359 automatically on startup. |
| Manual login | `AuthRepository.kt:65` | `JellyfinClient::authenticate_by_name` | `VERIFIED` | Full username + password auth over HTTPS / LAN. |
| Quick Connect | `AuthRepository.kt:102` | `JellyfinClient` QuickConnect flow | `VERIFIED` | Initiate, poll, cancel, and complete authentication. |
| Session restoration | `AuthDataStore.kt` | `CredentialStore` + `LocalStore` | `VERIFIED` | Encrypted store + SQLite session caching. |
| Cold launch landing screen | `MainActivity.kt` | Home dashboard; media opens only when explicitly passed on the command line | `IN_PROGRESS` | Verified in a fresh Linux Xvfb profile; packaged Windows startup still needs native runtime verification. |
| Logout & clear session | `AuthRepository.kt:152` | `JellyfinClient::logout` + clear | `VERIFIED` | Clears active token and memory state. |
| Library selection | `LibraryRepository.kt` | `JellyfinClient::get_user_views` | `VERIFIED` | Dynamic view switching from top navigation bar. |
| Service connection diagnostics | `SettingsScreen.kt` | Settings status for Jellyfin and Resolver | `IN_PROGRESS` | A manual check reports reachability, library/category counts, and forces fresh network fetches of up to five Jellyfin movie posters plus one Resolver image. Jellyfin status reports loaded posters and missing URLs; redacted diagnostics include image type and HTTP status. Native runtime verification against user servers remains pending. |

---

## 2. Discovery & Browsing

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Continue Watching | `LibraryRepository.kt:resume` | `JellyfinClient::get_resume_items` | `VERIFIED` | Resume playback from saved Jellyfin & Telegram positions. |
| Section loading & empty states | `HomeScreen.kt` / `SectionRow` | Home shelves show card skeletons during refresh, followed by clear empty-state cards for Resume, Latest, Random, Performers, and selected categories | `IN_PROGRESS` | Skeleton cards now preserve the home layout while library requests are running; runtime verification with a slow/empty server remains pending. |
| Home request errors & retry | `HomeScreen.kt` / `SectionRow` | Each home shelf reports its own Jellyfin request failure and offers retry; category loading errors use a home-level banner | `IN_PROGRESS` | Resume, latest, random, performer, and selected-category failures are preserved with section retry actions; runtime verification against a partially failing server remains pending. |
| Latest items | `LibraryRepository.kt:latest` | `JellyfinClient::get_latest_items` | `VERIFIED` | Chronological latest additions with auto-refresh. |
| Movie catalog | `LibraryRepository.kt:movies` | Movie grid with automatic near-end pagination and a manual fallback | `IN_PROGRESS` | Cards keep artwork clear with compact favorite/watched icons and source/studio in the metadata line. Sorting and quick filters are under one Options control; performer A–Z/photo filters and Open File are tucked into their secondary controls. ListView requests the next page near the end, guarded against duplicate requests; initial, search, performer-photo, and load-more requests fetch up to three further pages when filters leave the current batch empty, matching Android's filtered-page behavior. Genre, studio, and performer filters can be chosen from searchable Jellyfin option lists or entered directly. Native Windows scroll behavior remains unverified. |
| Item Detail Modal | `ItemDetailDialog.kt` | `ItemDetailDialog` Slint modal overlay | `VERIFIED` | Title, year, duration, resolution, studio, synopsis, cast pills. |
| Resume from Detail | `DetailScreen.kt` | Direct Resume vs Play from Start | `VERIFIED` | Dynamic `Resume (mm:ss)` and `Play from Start` actions. |
| Favorites Sync | `ItemDetailDialog.kt` | Favorite toggle (`❤️`) | `VERIFIED` | Local UI state + Jellyfin POST/DELETE `/Users/{id}/FavoriteItems/{id}`. |
| Watched Sync | `ItemDetailDialog.kt` | Played toggle (`✓`) | `VERIFIED` | Local UI state + Jellyfin POST/DELETE `/Users/{id}/PlayedItems/{id}`. |
| Surprise Me | `AllItemsTab.kt`, `LibraryViewModel.kt:onSurpriseMeClicked` | Filter-aware random pick in Browse | `IN_PROGRESS` | Random Jellyfin query honors favorites, unwatched, genre, studio, performer, and HD filters; local duration/excluded-tag filters are applied, and the selected title opens in its detail dialog. Falls back to loaded results; native Windows interaction remains unverified. |
| Performer Profiles | `PersonRepository.kt` | `PerformerProfileDialog` modal overlay | `VERIFIED` | Avatar, bio from resolver, nationality, birth, aliases, full filmography card list. |
| Studio Profiles | `StudioRepository.kt` | `StudioProfileDialog` modal overlay | `VERIFIED` | Studio badge, release counter, and studio catalog releases list. |
| Similar Items | `DetailScreen.kt` | "More Like This" recommendations | `VERIFIED` | Weighted ranking by studio (+5), performers (+4), genres (+3) in detail dialog. |
| Performer drilldown | `PersonRepository.kt` | Performer chips + filter | `VERIFIED` | Click performer chip in detail to filter catalog. |
| Studio drilldown | `StudioRepository.kt` | Studio badge + filter | `VERIFIED` | Click studio badge in detail to filter catalog. |
| Categories & chips | `LibraryRepository.kt:genres` | Category chips and genre drilldown | `IN_PROGRESS` | Latest, Resume, Favorites remain primary browse chips; Watch History, Channels, Performers, Studios, and Categories are grouped under More, with the active section still shown on the collapsed button. Choosing a genre opens its filtered movies with a category heading and a direct Back to Categories action. Native Windows navigation remains unverified. |
| Performer browse & search | `ActorsTab.kt`, `LibraryViewModel.kt:setActorSearchQuery` | Performers search field + compact `ALL` / `A–Z` row + photo filter | `IN_PROGRESS` | Letter selection refreshes the Jellyfin actor query; name search is debounced and paginated; photo-only filtering remains available. Desktop UI compiles in the Windows release build; native interaction verification remains pending. |
| Search & history | `SearchRepository.kt` | Debounced parallel search + recent queries + local result filters | `IN_PROGRESS` | Search fetches Jellyfin library items, performers, and Resolver channel videos concurrently, with a 300 ms input debounce and per-type result chips. Search mode hides unrelated catalog controls, shows search-specific loading/empty/error states, offers retry when every source fails, preserves successful movie/performer/channel results when another source fails, fetches additional pages when exclusions leave the batch empty, and filters cached results without another request. Ctrl+F focuses the catalog search field, or performer search while browsing Performers. Live Jellyfin/Resolver behavior and full keyboard navigation still need runtime verification. |

---

## 3. Channels & Resolver

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Channels Screen | `ChannelsFragment.kt` | `ChannelsScreen` top-level tab | `VERIFIED` | Split-view: Left sidebar categories list + Right video stream. |
| Resolver categories | `ResolverApi.kt:12` | `ResolverClient::get_categories` | `VERIFIED` | Dynamic channels & categories list with offline fallbacks. |
| Category videos | `ResolverApi.kt:15` | `ResolverClient::get_category_videos` | `VERIFIED` | Paginated channel items. |
| Telegram history & pagination | `ResolverApi.kt:23` | `ResolverClient::get_channel_history` | `IN_PROGRESS` | Cursor pagination via `next_offset_id`; desktop auto-requests the next batch near the end of the list and fetches up to three additional pages when exclusions leave the first page empty, matching Android's empty-filtered-page behavior. Native Windows scroll behavior remains unverified. |
| Channel search | `SearchRepository.kt` | In-channel search input | `VERIFIED` | Instant live filtering within active channel. |
| Resolver search | `ResolverApi.kt:30` | `ResolverClient::search` | `VERIFIED` | Search across telegram/web providers. |
| Performer biography | `ResolverApi.kt:36` | `ResolverClient::get_performer_bio` | `VERIFIED` | Fallback bio provider. |

---

## 4. Playback (Adult Content Tailored) & HTPC Navigation

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Embedded video | `PlayerActivity.kt` | libmpv OpenGL in Slint window | `IN_PROGRESS` | The bundled MP4 renders visible frames in the Slint window under Linux Xvfb/Mesa llvmpipe, and the timeline advances with the playback controls visible. The player uses a compact timeline/transport bar; seek, play/pause and volume stay in the compact bar; other controls and direct-file loading expand under More, with advanced playback options nested one level further. The overlay fades during playback. Native Windows rendering and hardware playback still need verification. |
| Playback error recovery | `PlayerScreen.kt` / `PlaybackErrorDialog` | Centered playback error panel with retry and browse actions | `IN_PROGRESS` | Retry reloads the remembered libmpv source with its original HTTP headers and current position; hardware-decode failure retries direct URLs and local files in software mode. libmpv warning/error logs are included in redacted diagnostics. Native runtime verification pending. |
| Local files & direct URLs | Desktop enhancement | Windows file picker and direct media URL/path entry in Player | `IN_PROGRESS` | Direct sources bypass Jellyfin stream resolution and credentials; explicit startup media waits for renderer readiness. The Windows picker includes common camera/container formats and M3U/HLS playlists; playback and native file-dialog verification on Windows remain pending. |
| Autoplay next | `SettingsScreen.kt` / `PlayerViewModel.kt` | Player toggle with 10-second random-next countdown | `IN_PROGRESS` | The preference now loads at startup, persists when toggled, and stays synchronized after importing configuration. Countdown behavior and selection still need Windows runtime verification. |
| Play / Pause | `PlayerControls.kt` | Spacebar, Play/Pause button | `VERIFIED` | Responsive pause/resume toggling. |
| Seeking | `PlayerControls.kt`, `PlayerOverlay.kt` | Seekbar, step seek buttons, Arrow keys, `[` / `]` | `IN_PROGRESS` | Arrow keys and step buttons accelerate repeated same-direction seeks up to 60 seconds within a 1 s repeat window and show a centered 1.5 s seek confirmation; native Windows interaction verification remains pending. |
| Volume & Mute | `PlayerControls.kt` | Volume slider, Mute toggle, 'm' / '0', Up/Down | `VERIFIED` | Volume +/- 5% step, clamped [0.0, 100.0], '0'/'m' mute. |
| Playback Speed | `PlayerOverlay.kt`, `PlayerScreen.kt` | Cycle 1×, 1.25×, 1.5×, 2× | `IN_PROGRESS` | Resets to 1× for each new Jellyfin, Telegram, and local video, matching Android; retrying the current video preserves speed. Native Windows playback verification remains pending. |
| Mouse Canvas Gestures | Desktop Enhancement | Double-click, wheel, right-click | `VERIFIED` | Double-click fullscreen, mouse wheel volume, right-click HUD stats. |
| HTPC Remote Shortcuts | Android TV D-Pad | FocusScope keyboard bindings | `VERIFIED` | Arrows for seek & volume, 1-4 for quick screen switch, hierarchical Esc. |
| Aspect ratio | `PlayerControls.kt` | Cycle aspect button (Auto, 16:9, etc.) | `VERIFIED` | Verified with `video-aspect-override`. |
| Stats HUD | `PlayerControls.kt` | HUD overlay, 'i' key, canvas right-click | `VERIFIED` | Resolution, codecs, fps, dropped frames. |
| Subtitles & Audio tracks | `PlayerScreen.kt` (`PlayerView.useController = false`), `PlayerOverlay.kt` | **Not exposed** | `VERIFIED` | Android TV disables the built-in controller and its custom overlay has no track selector; desktop omission matches the reference. |
| Playback reporting & close checkpoint | `PlaybackRepository.kt`, `PlayerViewModel.kt:onExitPlayer` | Jellyfin playback session reporting + native window close handling | `VERIFIED` | Start, Progress, Stop events with play method and position tracking; leaving Player or closing the window snapshots progress and stops the session. |
| Panic Exit | Desktop Enhancement | Emergency panic shortcut | `VERIFIED` | Ctrl+Shift+Q immediately hides window, mutes audio, and exits. |
| pHash matching | Desktop Enhancement | Perceptual hashing coordinator | `VERIFIED` | Offline background worker enqueuing hashes for content deduplication. |

---

## 5. Local Storage & Security

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Credentials storage | Android Keystore | `CredentialStore` + `LocalStore` | `VERIFIED` | Secure local token management. |
| Settings & histories | Room / DataStore | SQLite (`rusqlite`) with migrations | `VERIFIED` | Search history, channel preferences, local progress. Clearing Telegram resume progress refreshes the previous content screen while retaining Jellyfin's server-backed resume items; clearing watch history reloads the Home history shelf. |
| Thumbnail cache size & disable option | `SettingsScreen.kt` | Settings cache cycle + bounded thumbnail cache | `IN_PROGRESS` | Supports Off, 128, 256, 512, and 1024 MB; cache now persists under Windows `LOCALAPPDATA` and Linux `XDG_CACHE_HOME`/`~/.cache`, and is pruned to the configured limit. Jellyfin list requests explicitly enable Primary/Thumb/Backdrop images; film cards now prefer Thumb, then Backdrop, then Primary when corresponding tags exist, matching Android, while retaining the remaining image types as fallbacks and skipping blank or near-black candidates. URLs use tags when supplied, retry without stale tags, and use untagged URLs when metadata omits them. Requests retry transient connection/timeout errors, interrupted or empty bodies, and HTTP 408/429/5xx up to twice before trying the next image type. Backdrop requests try Android-compatible `/Images/Backdrop` first, then `/Images/Backdrop/0` for servers that require an index. Item image URLs match Android's Jellyfin request shape without forcing an output format. Redacted diagnostics record HTTP status, connection/timeout, and decode failures as well as successful download/decode. Resolver relative image paths and stale `/admin/` origins are normalized to the configured Resolver URL, preserving reverse-proxy path prefixes and matching Android; home shelves, catalog, and channel cards refresh progressively as images arrive without replacing list models; workers pass encoded image bytes to the UI, so artwork remains visible when persistent caching is Off. Filtered catalog subsets retain their updates even when the paging cache has additional items. Verify against the user's Jellyfin/Resolver server. |
| Global excluded tags | `LibraryViewModel.kt`, `SettingsScreen.kt` | Excluded-tag settings applied to home, library, search, and channel content | `IN_PROGRESS` | Items are filtered by category/genre, metadata tag, title, and performer, matching Android's exclusion rules. Editing the exclusion list refreshes the last content screen immediately; runtime verification remains pending. |
| Low-memory thumbnail mode | `SettingsScreen.kt:382`, `ThumbnailQuality.kt` | Low-RAM toggle selects 320 px catalog images and 200 px person portraits instead of 480 px and 300 px, refreshing the last content screen when changed from Settings | `IN_PROGRESS` | Catalog and performer profile image sizing now follow the persisted preference; changing performer letter/photo filters re-fetches artwork for the new card set. Home shelves, catalog, and channel grids update image rows progressively without replacing their list models, so early thumbnails appear before all requests finish and scroll position is preserved. Changing Low-RAM mode in Settings now refreshes the content screen the user came from, including channels. Redacted diagnostics record failed requests, decode errors, and successful downloads. Native Windows runtime verification against the user server remains pending. |
| Poor Telegram thumbnail fallback | `MovieCard.kt:70-143`, `ItemDetailScreen.kt:177-216`, `ThumbnailQuality.kt` | Detect missing, black, or undersized artwork and request Resolver pHash thumbnail in cards and detail artwork | `IN_PROGRESS` | Desktop requests pHash artwork for Telegram cards, filmography, similar items, and detail artwork; persisted jobs are scoped by Resolver origin and account-token fingerprint so they are not resumed across accounts; resumed items preserve their saved Resolver thumbnail and prefer pHash when needed, while missing, undersized/dark, or failed primary artwork falls back. Runtime verification against Resolver remains pending. |
| Device & hardware diagnostics | `SettingsScreen.kt` | Windows/Linux system information card + Vulkan loader probe + libmpv decoder availability + redacted diagnostics export | `IN_PROGRESS` | Displays OS, CPU, RAM, Vulkan loader availability, H.264/HEVC/VP9/AV1 availability, and hardware-decode preference; codec listing does not probe GPU decode support. Native Windows runtime verification remains pending. |
| Config Export / Import | Desktop Enhancement | JSON settings backup | `VERIFIED` | Atomic export and schema-validated import. |
| Diagnostics Export | Desktop Enhancement | Redacted diagnostics log | `VERIFIED` | Masked tokens, sanitized paths, and system stats. |

---

## 6. Distribution & Packaging

| Android Feature | Android Reference | Desktop Equivalent | Status | Evidence / Notes |
| --- | --- | --- | --- | --- |
| Application Delivery | Android APK / TV build | Windows x64 EXE with adjacent pinned libmpv DLL | `IN_PROGRESS` | Current build emits `bin/adult-hub-desktop.exe` and `bin/libmpv-2.dll`, without setup or ZIP. Cross-compilation succeeded; native Windows launch/playback remain unverified. |
| In-App Auto-Update | App Store / Sideload | Ed25519-signed EXE and runtime DLL update | `IN_PROGRESS` | The Windows manifest signs and identifies both the EXE and adjacent runtime DLL. The updater verifies both files, replaces them after shutdown and rolls back on startup failure. Native Windows handoff and a published signed release remain unverified. |

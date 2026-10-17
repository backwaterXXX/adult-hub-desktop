# Stage 6 — Deep Discovery & Item Detail Parity Evidence

## 1. Overview
Stage 6 delivers deep discovery and full feature parity with Android TV (`/app/adult-hub-tv/app/src/main/java/com/adulthub/tv/ui/detail/` and `library/`). The desktop application now provides an interactive item detail dialog, favorites and watch status synchronization, performer and studio drilldown exploration, dedicated category filtering, and instant card playback alongside deep metadata browsing.

Per user constraints, Linux packages were not built, and all release artifacts were compiled directly for Windows and placed into `bin/`.

---

## 2. Implemented Capabilities & UI Components

### 2.1 Interactive Item Detail Modal (`ItemDetailDialog`)
- **Visual Design**: Dark modal scrim (`#000000e6`) with centered dialog container (`#141418`, neon pink border `#e91e6388`, rounded corners `12px`).
- **Metadata Fields**:
  - Item Title (word-wrapped, 20px bold)
  - Release Year
  - Formatted Duration (`Xh Ym` / `Xm Ys`)
  - Resolution (`width x height`, e.g., `1920x1080`)
  - Studio pill badge (clickable for instant studio catalog filter)
  - Genres row
  - Scrollable Overview / Synopsis (`Flickable` multiline text)
  - Cast & Performers pills (clickable for instant performer catalog filter)
- **Detail Actions**:
  - `▶ Play` / `▶ Play from Start`: Launches video from beginning (0:00).
  - `Resume (mm:ss)`: Appears dynamically when saved progress $\ge 15\text{s}$, resuming directly at the checkpoint.
  - `❤️ Favorited` / `🤍 Favorite`: Toggles favorite state locally and syncs to Jellyfin API asynchronously.
  - `✓ Played` / `Mark Watched`: Toggles watch status locally and syncs to Jellyfin `/Users/{user_id}/PlayedItems/{item_id}`.
  - `Close` / `✕`: Dismisses the modal and restores focus.
- **Dismissal Controls**:
  - Click on backdrop scrim outside dialog closes modal.
  - `Escape` key dismisses detail dialog with top precedence before screen navigation.

### 2.2 Video Card Enhancements (`MovieCardItem`)
- **Card Click vs. Play Button**:
  - Clicking card body opens the `ItemDetailDialog` with rich metadata and cast pills.
  - Clicking the circular play button (`▶`) launches instant playback without opening the dialog.
- **Status Badges**:
  - Dynamic `❤️` badge displayed on thumbnail preview for favorited items.
  - `WATCHED` badge displayed on thumbnail preview for completed/marked items.
  - Studio name displayed in metadata row alongside year and provider badge (`JF` / `TG` / `LOCAL`).

### 2.3 Category Discovery Chips
- Added interactive category chips in Browse top bar:
  - **Latest**: Recently added media items from Jellyfin / local library.
  - **Resume**: Items in progress with saved checkpoints ($> 15\text{s}$).
  - **Channels**: Telegram video items and channels.
  - **Favorites**: Filtered view of items marked as favorites (`is_favorite = true`).
  - **Performers**: Items featuring cast and performer metadata.
  - **Studios**: Items categorized by studio.

### 2.4 Client API Extensions (`desktop/crates/client`)
- **`JellyfinClient::toggle_played`**:
  - Endpoints: `POST` / `DELETE` to `/Users/{user_id}/PlayedItems/{item_id}`.
  - UUID validation to protect against invalid item IDs.
  - Custom query support: added `person`, `studio`, `genre`, and `is_favorite` parameters to `GetItemsQuery` and `JellyfinClient::get_items`.

---

## 3. Generated Windows Release Artifacts
All Windows binaries and packages built and verified in `bin/`:

| Artifact | Type | Size | SHA-256 Checksum |
| :--- | :--- | :--- | :--- |
| `adult-hub-desktop.exe` | Windows x64 Stripped Executable | 32 MB | `2df92cf7dca32607593c6be4b46c2d49b291d293fd695efad190772f7e7f7b19` |
| `adult-hub-desktop-setup.exe` | Windows x64 NSIS Installer | 60 MB | `b386933f47984c82c399137e7ac87abb6630f58c2921ef6669a4ad32e4147643` |
| `adult-hub-desktop-windows-x64.zip` | Windows x64 Portable ZIP Bundle | 60 MB | `5e597b1020f557bd4cc62a5e9493b16974d81db2513869d91a4b8552b9600c65` |
| `adult-hub-update-manifest.json` | Signed Release Manifest (v1) | 1.7 KB | `cbff10586e0fc21e42a03c2763f03b22e1755ee3cb8210352ef2049d5cbf23eb` |
| `adult-hub-update-manifest.json.sig` | Detached Ed25519 Signature | 64 B | `9b363435...` |

---

## 4. Test Verification
All 62 unit and integration tests across the desktop workspace pass:
- `adult_hub_client`: 18 tests passed (including UDP server discovery, Jellyfin URL normalization, and playback start info).
- `adult_hub_desktop`: 27 tests passed (including `tv_item_to_card_mapping` verifying `is_favorite` and `studio`, session management, updater signatures, and diagnostics redaction).
- `adult_hub_player`: 6 tests passed (including buffer profiles, MPV headers, HUD formatting).
- `adult_hub_storage`: 11 tests passed (including progress tracking, sessions, settings CRUD, and config import/export).

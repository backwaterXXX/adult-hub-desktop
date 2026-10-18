# Adult Hub Desktop — General Plan

Status: Core implementation and Windows packaging are in place. Release acceptance remains open pending native Windows installation, playback, and integration checks, plus the platform coverage listed below.

## Goal and agreed defaults

Create a functional port of Adult Hub TV and Mobile for desktop computers, with the same capabilities and visual identity adapted to a mouse, keyboard and resizable window.

- Foundation: Rust application logic, Slint UI and embedded libmpv playback.
- Platforms: Windows 10 22H2 and Windows 11 on x86_64; Ubuntu 24.04 LTS and a Fedora release supported at release time on x86_64.
- Linux window systems: X11 and Wayland. Record the exact Fedora release in release validation.
- Display output: SDR, including tone mapping of HDR sources to SDR; native HDR output is deferred.
- Distribution: Windows is delivered as the EXE and the adjacent pinned libmpv runtime DLL, without an installer or archive; Linux retains AppImage and `.deb` package options.
- One desktop interface. A separate TV mode, ARM64, macOS, Microsoft Store and Flathub are deferred.
- Android remains Kotlin + Compose + Media3. A shared Rust core may be considered later when concrete duplication justifies the integration cost.

## Technology recommendation

| Approach | Assessment |
| --- | --- |
| Rust + Slint + libmpv | Selected: Rust foundation with a declarative custom media UI and an embedded playback engine. |
| Kotlin + Compose Desktop + libmpv | Strong alternative if sharing existing Android logic becomes the overriding priority. |
| Rust + Tauri + libmpv | Feasible, but adds integration between native video and platform-dependent WebViews. |
| Rust + egui + libmpv | Viable custom-rendering alternative; Slint is the selected UI approach. |

Slint exposes OpenGL texture import, while libmpv exposes an embedding render API. This makes a shared OpenGL texture a plausible integration path, not an already validated implementation. Stage 1 must prove it on both operating systems before full screen development.

The application will reuse behavior and API contracts from Android, rather than Kotlin source code. Moving application logic to Rust alone is not expected to improve codec performance; decoding remains the responsibility of libmpv, its dependencies and the device drivers.

## Baseline feature parity

Use Android revision `37deee2` as the initial reference. During Stage 2, create a feature checklist with implementation status and validation evidence for each capability below.

| Area | Required capabilities |
| --- | --- |
| Connection and identity | Server entry, manual login, Quick Connect, saved session, logout and library selection. |
| Discovery | Home sections, Continue Watching, local history, latest items, random discovery and filtered shuffle. |
| Library | Movies, categories, performers, studios, detail pages, biographies, filmographies, similar items, favorites and watched status. |
| Search and channels | Search history, filtering, sorting, pagination, Resolver channels and Telegram history. |
| Playback | Embedded video, resume rules, seek, speed, aspect modes, audio/subtitles, autoplay, random playback, buffering profiles, low-memory mode, recovery and statistics HUD. |
| Progress | Jellyfin playback reporting and separate local Telegram progress. |
| Settings and privacy | Resolver override, category selection, hidden channels, excluded tags, cache controls, history clearing, connection checks, panic exit and secure session storage. |
| Supporting features | Compatible non-secret configuration format, diagnostics export, pHash submission/polling/recovery and in-app update checks/downloads. |

Preserve behavior implemented in source even where README summaries are incomplete. Android-specific operations receive desktop equivalents: for example APK installation becomes desktop package installation, and KeyStore becomes platform-backed credential protection.

## Architecture and interfaces

Create an independent Cargo workspace under `desktop/` in this project root. Separate application/domain logic, API clients, player integration, persistence and Slint UI. Keep build and release paths independent of Android Gradle tasks.

Internal boundaries:

- `JellyfinClient`: authentication, discovery, user actions, playback resolution and reporting.
- `ResolverClient`: categories, channel videos/history, search, performer biography and pHash jobs.
- `PlayerEngine`: load, pause/resume, seek, stop, track/speed/aspect selection, buffer configuration and playback events.
- `CredentialStore`: secure session persistence, retrieval and removal.
- `LocalStore`: settings, local histories, Telegram progress and pending pHash jobs.

API clients map transport DTOs into application models. Playback resolution creates a request containing the source URL, required headers, item/source/session identifiers, position, duration and reporting metadata. UI actions flow through application state to services; service events update application state and then UI models.

Network and storage work must not block the UI. Cancel superseded work and reject stale responses when a user changes screens, search terms or videos. Isolate libmpv unsafe FFI and keep render-context ownership explicit.

Use SQLite with versioned migrations for local data. Scope account-specific data by server/user; pHash records are scoped by normalized Resolver origin and a SHA-256 account-token fingerprint, without persisting the token itself. Preserve the Android non-secret configuration JSON version 1; credentials are excluded from export and HTTP consent is managed locally.

Protect tokens and saved session metadata with encryption using a key held by Windows Credential Manager or Linux Secret Service. If the secure store is unavailable, offer a non-persistent session; never silently save credentials in plaintext.

The existing Jellyfin and Resolver contracts are consumed without backend changes. Preserve identifiers, pagination, stream headers and Resolver URL rewriting. Maintain Android's service URL policy and explicit local HTTP consent.

## Delivery stages

1. [Player prototype](stage-01-player-prototype.md): embedded rendering and cross-platform playback feasibility.
2. [Authentication and browsing](stage-02-auth-and-browsing.md): API clients, persistence and discovery screens.
3. [Playback parity](stage-03-playback-parity.md): complete playback rules, reporting and local progress.
4. [Desktop UI and supporting features](stage-04-desktop-ui-and-features.md): interaction, settings, privacy, diagnostics and pHash.
5. [Packaging and release](stage-05-packaging-and-release.md): native packages, dependencies, updates and final acceptance.

Each stage document defines its deliverables, failure handling, validation and exit criteria. An unresolved Stage 1 rendering failure blocks full implementation and requires a revised integration plan.

## Distribution and licensing

Bundle pinned native playback dependencies. Keep project-owned code under Apache-2.0 and use Slint's royalty-free desktop route with the required attribution. Select libmpv/FFmpeg builds with license terms compatible with the intended distribution, and ship the applicable notices, source/build information and replacement materials required by those builds. Do not assume an arbitrary prebuilt library has the same licensing configuration.

Desktop CI and release tags are independent of Android. Updates provide release notes, download verification and user-confirmed installation. Windows replaces the single EXE after the application exits; `.deb` installation uses the system package mechanism; AppImage replacement occurs after exit through a helper.

## Validation and release criteria

- Every feature checklist entry has passing evidence on desktop.
- Fixture-based contract and domain tests cover missing fields, identifiers, pagination, URL rewriting, authentication errors and malformed responses.
- Playback validation covers representative H.264, HEVC, VP9 and AV1 sources; 1080p/4K; audio/subtitle tracks; and HDR-to-SDR output. Hardware decode is required only where the codec/profile is supported by the reference hardware.
- Test Windows 10/11, Ubuntu and Fedora, including Linux X11/Wayland and representative Intel, AMD and NVIDIA hardware. Publish exact hardware/driver coverage rather than claiming all combinations.
- Verify clean-machine installation, upgrades, secure-store failure, update rejection and interrupted downloads.
- Keep playback startup timing, dropped frames and resource measurements in local diagnostics. No remote analytics are introduced.
- Run backend-affecting scenarios against test doubles. Production backend databases remain outside the validation workflow.

## Documentation references

- [Slint OpenGL texture import](https://docs.slint.dev/latest/docs/rust/slint/struct.BorrowedOpenGLTextureBuilder)
- [Slint rendering lifecycle](https://docs.slint.dev/latest/docs/rust/slint/enum.RenderingState)
- [libmpv rendering API](https://github.com/mpv-player/mpv/blob/master/include/mpv/render.h)
- [mpv manual](https://mpv.io/manual/stable/)
- [Slint licensing options](https://slint.dev/get-started)
- [Tauri process model](https://v2.tauri.app/concept/process-model/)

These references informed the recommendation. Recheck current documentation and exact dependency versions at implementation time.

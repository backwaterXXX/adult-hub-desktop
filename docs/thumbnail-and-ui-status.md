# Adult Hub Desktop UI repair status

## Architecture and thumbnail diagnosis

The desktop client uses Slint 1.18.1. The main window and reusable views are
in `desktop/crates/app/ui/main.slint`; Rust callbacks and background work live
under `desktop/crates/app/src/handlers/`. The shared media-card model and its
catalog/channel row models are built in `desktop/crates/app/src/catalog.rs`.
Resolver/Jellyfin DTO conversion is in
`desktop/crates/client/src/models.rs`, image URL resolution, HTTP, decoding,
and disk cache are in `desktop/crates/app/src/thumbnails.rs`, and mpv is
wrapped by `desktop/crates/player` and rendered by the main Slint view.

The source-level cause addressed for Telegram thumbnails was that the desktop
image request path did not attach the active Resolver `X-Emby-Token`, although
the Resolver pHash API uses that authentication header. Telegram thumbnail
URLs point at the Resolver admin thumbnail endpoint, so this could explain
403 responses. The token is now attached only to the configured Resolver
origin and is not forwarded across redirects. Resolver URLs are resolved
against the configured server, and image response validation/decode failures
remain failures rather than being represented as missing artwork.

This is a confirmed code-path defect and correction, but the specific live
403 response for the previously observed record was not captured in this
environment. The available environment has no Windows or Wine runtime and no
configured Resolver/Jellyfin test account, so it cannot provide an anonymized
live download trace or prove a real Telegram image rendered. A passing build
does not establish that runtime result.

Other thumbnail protections include bounded concurrency (four downloads),
request deduplication, credential-separated dedup keys, terminal failure
states, explicit cache invalidation on Retry, image-size/decode limits,
redacted diagnostics, and stale-result matching by the item ID, provider, and
thumbnail source set. Retry now carries the selected card record through the
shared Home/Browse/Channels callback, so an ID collision cannot retry a
different source. Older view generations stop scheduling subsequent batches;
results from in-flight stale batches are discarded. Home also resolves each
attempted request to a terminal `ready`, `failed`, or `missing` state, including
decode failures and records with no source URL.

## Checks completed here

- `rustfmt --check` on changed Rust files and `git diff --check`: passed.
- Windows GNU release build: passed; `bin/adult-hub-desktop.exe` is a 64-bit
  Windows GUI executable with the `.ico` resource embedded. SHA-256:
  `b06344a2c327579c084b7b0c679b8e52da65daa89a2b9274e9eec9ea8bc15497`.
- Windows-targeted GUI test compilation (`cargo test ... --no-run`): passed.
  Running the Windows test executable is not available on this Linux host.
- Client tests: 20 passed, 1 ignored (requires a live Jellyfin service), and 1
  excluded because this sandbox denies UDP socket creation. Running all tests
  without excluding it reproduces that environment permission failure.
- Storage tests: 11 passed.
- Native Linux GUI tests could not be built because `pkg-config` and the
  system `fontconfig` development package are absent.
- Runtime screenshots, Windows DPI checks, real thumbnail downloads, and mpv
  fullscreen/input checks: not verified here. The step-by-step checklist is in
  `docs/windows-runtime-verification.md`.

## Acceptance status

| Requirement | Status |
|---|---|
| Responsive card sizing, aligned section headers, compact Browse/Channels layouts | Built; Windows runtime not verified |
| Shared card and view states, neutral title fallback, retry and cache handling | Confirmed by source and targeted tests; Windows runtime not verified |
| Resolver thumbnail authentication and safe redirect scope | Confirmed by source and targeted tests; live Resolver result not verified |
| Empty Player state, Settings layout, sidebar/icon resources, scoped status messages | Built; Windows runtime not verified |
| mpv playback, fullscreen and input layering | Not verified in runtime |
| Screenshots of Home, Browse, Channels, Player and Settings at requested sizes | Not captured; Windows runtime unavailable |

The target Windows executable includes the application icon. `build.rs` embeds
the ICO into the executable resource section, and `main.rs` assigns the same
asset to the native application window. Its Windows taskbar rendering still
needs a runtime check.

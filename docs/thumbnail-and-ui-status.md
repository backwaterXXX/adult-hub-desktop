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
redacted diagnostics, and stale-result matching by stable item ID. Older view
generations stop scheduling subsequent batches; results from in-flight stale
batches are discarded.

## Checks completed here

- `cargo fmt --all` and `git diff --check`: passed.
- Windows GNU release build: passed; `bin/adult-hub-desktop.exe` was rebuilt
  and is a 64-bit Windows GUI executable. Its SHA-256 is
  `b09c86fcf941d852706f3d814b85be3a3e98dd7fc93a8fb0618c55c8bd6eb514`.
- Windows-targeted GUI test compilation (`cargo test ... --no-run`): passed.
- Client tests: 20 passed, 1 ignored (requires a live Jellyfin service), and 1
  filtered (UDP discovery is blocked in this environment).
- Storage tests: 11 passed.
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

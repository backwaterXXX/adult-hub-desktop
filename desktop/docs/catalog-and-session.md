# Catalog and session behaviour (desktop 0.2.49+)

Short reference for the desktop client. The server contract is documented in
the backend repository (`docs/direct-play-migration/api-v1-contract.md` and
`docs/catalog-performance.md`).

## Session

The backend issues an access token valid for **1 hour** and a refresh token for
30 days.

- `install_session_keepalive` (`crates/app/src/main.rs`) refreshes the token
  every **8 minutes** while a session is present.
- `filter_category` and the other browse paths retry once after a `401` before
  showing the login modal.
- A startup refresh signs out only when the server definitively rejects the
  token (`Unauthorized`, `NotFound`) or no refresh token exists. A timeout or a
  `5xx` keeps the saved session; see `refresh_failure_signs_out`.

## Catalog filters and sorting

- Filters, sort and direction are stored per tab under the setting
  `catalog_filters_v1` (`handlers/browsing/filters.rs`).
- Sort options: Title (A–Z default), Latest, Duration; performers add Name A–Z,
  Name Z–A and Most videos. The dead Rating option is gone.
- Quality chips SD / HD / HQ send the server-side `quality` parameter; the
  client never derives quality from file size.
- Server-side category, tag, studio and performer matching is
  **case-insensitive**; the client sends the canonical value from the taxonomy.
- **Continue Watching is removed.** Watch History is the only history surface;
  resume lives on Home.

## Raw Telegram references

Titles stored as a bare `channel:message:file` reference are hydrated with the
`telegram:` prefix (`resume_title_needs_hydration`). Rows whose title is still a
bare reference after hydration are hidden from Home resume/history and the Watch
History page (`has_unresolved_raw_title`, `handlers/home_categories.rs`). They
stay in local storage, so a later catalog recovery brings them back with real
metadata.

## Where to look

| Concern | File |
|---|---|
| Startup, keepalive, sign-out rules | `crates/app/src/main.rs` |
| Browse filters, hydration, hiding | `crates/app/src/handlers/browsing/mod.rs` |
| Filter persistence and sort enums | `crates/app/src/handlers/browsing/filters.rs` |
| Raw-title detection | `crates/app/src/handlers/home_categories.rs` |
| Download session refresh | `crates/app/src/download.rs` |
| Slint UI | `crates/app/ui/main.slint` |

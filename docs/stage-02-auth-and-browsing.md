# Stage 2 — Authentication and Browsing

Status: not started. Depends on Stage 1 acceptance.

## Objective

Implement the client and persistence foundation, then reproduce Android authentication and content discovery using desktop screens.

## Deliverables and tasks

- Create an English feature parity checklist from Android revision `37deee2`. Include capabilities from both TV and mobile, with source references, desktop equivalents and acceptance evidence fields.
- Implement `JellyfinClient` and `ResolverClient` with transport DTOs separated from application models. Derive endpoint behavior from the existing clients and fetched official API documentation.
- Implement server validation, manual login, Quick Connect, session restoration, logout and library selection. Use a stable desktop device identifier distinct from Android.
- Implement `CredentialStore` and SQLite-backed `LocalStore`, including schema versioning and transactional migrations.
- Port Home sections, Continue Watching, history, latest items, random discovery and filtered shuffle.
- Port movie/category/performer/studio browsing, detail pages, biographies, filmographies, similar items, favorites and watched status.
- Implement search and search history, filters, sorting and pagination.
- Implement Resolver categories/channels, category videos, Telegram history and performer biography fallback.
- Build functional desktop screens sufficient to exercise these capabilities; complete visual polish in Stage 4.

## Contracts and data flow

Preserve Jellyfin UUIDs and provider video IDs as distinct identifier types. Keep provider IDs as strings to avoid numeric precision loss. Preserve existing null/default handling, thumbnail mapping and pagination semantics, including Telegram `offset_id` cursors.

For Resolver, retain the existing routes for categories, category videos, Telegram history, search and performer biographies. Do not invent backend endpoints or silently substitute one provider for another.

UI actions enter application state, then API services. Map results into screen state with loading, empty, partial-success and error states. Cancel superseded requests and use request identities so late results cannot overwrite newer navigation or searches.

Partition account-owned histories and progress by normalized server origin/user identity. Keep secrets out of SQLite plaintext. Store encrypted session metadata and tokens under a key protected by Windows Credential Manager or Linux Secret Service. A locked/unavailable store permits a session without persistence.

Reproduce `ServiceUrlPolicy`, including supported private/local HTTP hosts, origin-specific consent and HTTPS for public services. Resolver follows the Jellyfin address unless explicitly overridden. Treat image fetching as authenticated networking where required; redact credentials in its failures too.

## Failure handling

- Expired credentials return the user to authentication without mixing account data.
- Quick Connect can be cancelled, expires cleanly and stops polling on navigation.
- Resolver failure does not block independent Jellyfin content.
- Empty libraries and missing metadata have explicit UI states.
- Account/library changes invalidate stale requests and relevant cached content.
- Database migration failure preserves the previous database and reports the problem; do not silently erase user data.

## Validation

Use fixtures/test doubles for auth, Quick Connect, malformed responses, unavailable services, missing optional fields, pagination, URL policy and cross-account isolation. Exercise favorites/watched actions only against test doubles.

Check each discovery flow against the Android baseline, including filters, random exclusions, hidden channels and excluded tags. Confirm that saved sessions restore securely and unavailable secure storage never causes plaintext credential persistence.

## Acceptance checklist

- [ ] Feature parity checklist covers the complete Android baseline.
- [ ] Manual login, Quick Connect, restore/logout and library selection work.
- [ ] All discovery screens and user actions have working application/API flows.
- [ ] Resolver channels and Telegram pagination preserve identifiers and ordering.
- [ ] Stale requests and account changes cannot display the wrong account's content.
- [ ] Local storage, migration and secure-store failure scenarios pass.
- [ ] No backend code, scripts or production databases were modified.

Next: [Stage 3](stage-03-playback-parity.md).

# Stage 5 — Packaging and Release

Status: not started. Depends on Stages 1–4 acceptance.

## Objective

Deliver installable Windows and Linux packages, verified updates and evidence that the complete port meets its release criteria.

## Deliverables and tasks

- Add desktop CI for Rust checks, meaningful tests, release builds and package creation. Keep desktop jobs/tags independent of Android workflows.
- Produce a per-user Windows x64 installer, a Linux x64 AppImage and a `.deb` package for Ubuntu 24.04 LTS. Validate the AppImage on the selected Fedora release too.
- Bundle pinned libmpv and required native dependencies. Resolve bundled libraries from the application package, not arbitrary current working directories.
- Record native dependency versions, build options, checksums and redistribution requirements. Include project and third-party notices and required source/build/replacement materials.
- Use Slint's royalty-free desktop route with attribution; retain Apache-2.0 for project-owned code. Verify the actual libmpv/FFmpeg build licensing configuration before packaging it.
- Add application identity, icons, shortcuts, normal uninstall behavior and English release notes.
- Publish artifacts and update metadata through GitHub Releases using independent `desktop-vX.Y.Z` tags. Begin with a prerelease candidate; stable promotion requires the release checklist to pass.

## Update behavior and interface

Provide automatic checks controlled by the saved preference and a manual check action. Show the available version and release notes; downloading or installing an update requires the user's action.

Use a desktop-specific manifest with a schema version, application version, release notes URL, and platform/architecture/package records containing HTTPS download URL, SHA-256 and package size. Distribute a detached Ed25519 signature for the manifest and embed the verification public key in the application. The signed manifest authenticates each package hash; a checksum from the same unsigned download location is insufficient.

Reject invalid signatures, mismatched platform/package records, unexpected redirects outside configured release hosts and hash mismatches. Download into a temporary file, verify it and only then offer installation. Keep signing private keys in release secrets, outside source control.

Windows launches the verified installer after bounded playback/application shutdown. `.deb` updates hand off to the system package installer and its authorization flow. AppImage updates use a replacement helper after exit, retain a rollback copy until the replacement launches successfully and report read-only locations without attempting an unsafe overwrite.

Updates preserve existing application data. Schema migrations must preserve data and handle failure safely. Installation is never started during active playback without first confirming interruption.

## Platform and release validation

| Platform | Required coverage |
| --- | --- |
| Windows 10 22H2 x64 | Clean install, playback, secure storage, update and uninstall. |
| Windows 11 x64 | The same checks plus fullscreen/HiDPI behavior. |
| Ubuntu 24.04 LTS x64 | `.deb` and AppImage, X11/Wayland, secure storage and update behavior. |
| Supported Fedora x64 | AppImage, X11/Wayland, secure storage and update behavior. |

Record exact OS versions, GPU models and drivers. Cover representative Intel, AMD and NVIDIA configurations, clearly stating combinations not tested. The application must run without Rust, Android SDK, Gradle or manually installed development libraries.

Validate package upgrade with saved settings/history, locked/unavailable secure storage, interrupted downloads, invalid signatures, corrupted packages, read-only installation paths and failed AppImage replacement. Use a controlled update fixture or prerelease channel to avoid publishing test artifacts as stable releases.

Run the final parity checklist and representative codec/resolution/subtitle/tone-mapping matrix against packaged applications. Keep local evidence of startup timing, dropped frames, CPU/GPU use and memory behavior across extended playback and repeated source changes.

## Acceptance checklist

- [ ] All prior stages and parity checklist entries pass.
- [ ] Windows installer, AppImage and `.deb` run on clean supported systems.
- [ ] Bundled native dependencies are pinned, discoverable and accompanied by required materials.
- [ ] Update authenticity, download failure and installation recovery scenarios pass.
- [ ] Upgrades preserve settings, histories and secure sessions where the platform store remains available.
- [ ] Exact platform/GPU coverage and known limitations are documented.
- [ ] A release candidate passes validation before stable promotion.
- [ ] Stable publication is explicitly authorized at execution time; these planning documents do not publish anything.

## Post-release handling

Use local diagnostic exports for user-reported failures. Triage playback issues by source type, codec, OS, GPU and driver. Address release-blocking package/update failures before adding new platforms or a shared Android Rust core. Remote analytics are outside the agreed plan.

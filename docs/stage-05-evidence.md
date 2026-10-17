# Stage 5 — Packaging and Release Evidence

## 1. Overview
Stage 5 delivers the release packaging infrastructure, binary artifacts, in-app cryptographically verified update engine, CI/CD pipeline, and documentation.

## 2. Generated Release Artifacts
All artifacts are built and published into `bin/`:

| Artifact | Format / Platform | Size | SHA-256 Checksum |
| :--- | :--- | :--- | :--- |
| `adult-hub-desktop-setup.exe` | Windows x64 NSIS Installer | ~60 MB | `eb6e28a7e3b087281cc8d725e531f71d0ad0d502500b9fdb01d62597dfd283fc` |
| `adult-hub-desktop-windows-x64.zip` | Windows x64 Portable ZIP | ~59 MB | `4305cb5763728c17e9d5d0aa10921c17894a9665e0e875fe4b596a789c9b3d6a` |
| `adult-hub-desktop.exe` | Windows x64 Stripped Executable | ~31 MB | `2e08e8201a0bc0efd64eb07fe4ee09b8be00a6538b9d7e30d97960d7ee8f0200` |
| `adult-hub-desktop_0.1.0_amd64.deb` | Ubuntu 24.04 / Debian Package | ~11 MB | `8ca4ca35343639dafcc7cd1a181639bbcbdd5ecc1371104cdee90b615a474ead` |
| `Adult_Hub_Desktop-x86_64.AppImage` | Linux x86_64 AppImage | ~15 MB | `db6fc24c3542257070ca5cd8254fbbaeecf85a1f28d57e98bf61364458d8bff3` |
| `adult-hub-desktop` | Linux x86_64 Release Executable | ~39 MB | `60b943d0473b9347fb72a088ec13797961b7f94119d554a9d0ee761a66ffbe79` |
| `adult-hub-update-manifest.json` | Signed Release Manifest (v1) | 1.7 KB | `fc2cf69103e6bc465d836ea41203b0dff1ae6e625a6eb814d4ef633d26291a10` |
| `adult-hub-update-manifest.json.sig` | Detached Ed25519 Signature | 64 B | `3c09b0b1...` |

## 3. Native Dependencies & Pinned Libraries
- **Windows**: `libmpv-2.dll` (pinned build `accb7b2821` / 20261011, SHA-256 `68b6a82743...`) bundled directly alongside `adult-hub-desktop.exe`.
- **Package-relative loading**: `sys::MpvApi::load()` prioritizes the application package directory (`executable.parent()`) over arbitrary current working directories.
- **Licenses & Replacement**:
  - `LICENSE`: Apache 2.0.
  - `NOTICES.txt`: Slint royalty-free desktop attribution notice, libmpv LGPL/GPL terms and compile instructions, and FFmpeg redistributable notice.

## 4. In-App Update Engine (`updater.rs`)
- **Manifest Schema Version**: 1 (enforces strict schema rejection for versions $\ne 1$).
- **Signature Algorithm**: Detached Ed25519.
- **Embedded Public Key**: `c7201febbc61250b5627337868eaa3cebd36439da7275739a44f7ffcf07a8e43`.
- **Allowed Hosts**: `github.com`, `raw.githubusercontent.com`, `objects.githubusercontent.com`, `release.adult.ferges.eu`.
- **Integrity Check**: Bounded streaming download to `.staging` file, SHA-256 digest validation, and size comparison before promoting to executable installer.
- **Playback Interruption Safeguard**: Blocks updater launch during active playback.
- **AppImage Protection**: Verifies write permissions on parent directory; fails safely with `AppImageReadOnly` if mounted on read-only filesystem.

## 5. Automated Build & Packaging Scripts
- `./scripts/build.sh`: Universal orchestrator supporting `--all`, `--windows`, `--linux`, `--deb`, `--appimage`, `--installer`, `--manifest`.
- `./scripts/package-windows.sh`: Builds stripped Windows binary and zip bundle with `libmpv-2.dll`.
- `./scripts/package-windows-installer.sh`: Compiles per-user standalone NSIS installer (`makensis`).
- `./scripts/package-deb.sh`: Generates Debian / Ubuntu package (`dpkg-deb`).
- `./scripts/package-appimage.sh`: Builds standalone type-2 AppImage (`appimagetool`).
- `./scripts/sign-release.sh`: Hashes binaries, generates manifest, and signs detached Ed25519 signature.

## 6. Desktop CI Pipeline (`.github/workflows/desktop.yml`)
- Triggered on push to `main` and tags matching `desktop-v*`.
- Independent of Android mobile/TV workflows.
- Jobs:
  - `lint-and-test`: Formatting, clippy, unit & integration test matrix.
  - `build-linux`: Builds `.deb` and AppImage.
  - `build-windows`: Builds portable ZIP and NSIS installer.
  - `release`: Signs manifest and publishes GitHub release assets on tag push.

## 7. Test Suite Verification
All 62 workspace tests pass:
- 18 in `adult-hub-client`
- 27 in `adult-hub-desktop` (including manifest verification, version comparisons, host filtering, and hash checking)
- 6 in `adult-hub-player`
- 11 in `adult-hub-storage`

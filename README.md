# Adult Hub Desktop

A high-performance, privacy-focused native desktop media player and browser for Jellyfin and Resolver ecosystems, written in **Rust** with **Slint UI** and hardware-accelerated **libmpv** rendering.

Available for **Windows 10/11 (x86_64)** and **Linux (x86_64; AppImage & Debian/Ubuntu `.deb`)**.

---

## Architecture Overview

The repository is structured as a Cargo workspace located under `desktop/`:

| Crate | Path | Description |
|---|---|---|
| **`adult-hub-app`** | `desktop/crates/app` | Main application executable, Slint UI bindings, event handling, state coordination, and in-app updater. |
| **`adult-hub-client`** | `desktop/crates/client` | Resilient HTTP client for Jellyfin and Resolver APIs with validation policies, discovery, and DTO models. |
| **`adult-hub-player`** | `desktop/crates/player` | Narrow, safe wrapper around libmpv client and OpenGL rendering surface with thread isolation. |
| **`adult-hub-storage`** | `desktop/crates/storage` | SQLite local database with migrations, credential encryption, progress tracking, and non-secret config import/export. |

---

## Features

- **Embedded Hardware-Accelerated Video Playback**: Direct OpenGL FBO integration with `libmpv` supporting VA-API, NVDEC, and D3D11VA hardware decode.
- **Full Jellyfin Integration**: QuickConnect and password authentication, libraries, search, categories, resume progress, and playback session reporting.
- **Telegram / Resolver Channel Browsing**: Multi-source video streaming, channel filtering, and performer biography integration.
- **Smart Privacy & Panic Exit**: Instant `<Esc>` / Panic key binding that immediately blanks display, stops playback, and cleans up active sessions.
- **Video Fingerprinting (pHash)**: Background perceptual hashing coordination with server-side job tracking and status notifications.
- **HTPC & Remote Control Friendly**: Complete keyboard navigation support with volume clamping, seek stepping, and fullscreen toggling.
- **Verified In-App Updates**: Ed25519 cryptographic signature verification and SHA-256 package integrity checking before installing releases.

---

## Development Environment

Development is fully containerized to ensure reproducibility across developer environments without contaminating host system libraries.

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/)

### Building the Development Container

```bash
docker build -t adult-hub-desktop-dev:rust-1.99.1 \
  --build-arg USER_ID=$(id -u) --build-arg GROUP_ID=$(id -g) dev
```

### Running Commands in the Container

Use the `dev/run.sh` helper to run commands inside the container with host X11 and DRM pass-through:

```bash
# Start an interactive shell
./dev/run.sh

# Run unit and integration tests across the workspace
./dev/run.sh cargo test --manifest-path desktop/Cargo.toml

# Run clippy linter with strict warning checks
./dev/run.sh cargo clippy --manifest-path desktop/Cargo.toml --all-targets -- -D warnings

# Check code formatting
./dev/run.sh cargo fmt --manifest-path desktop/Cargo.toml --all -- --check
```

---

## Packaging & Releases

Packaging scripts are available in `scripts/`:

```bash
# Build Linux release binary and packages (.deb & AppImage)
./scripts/package-deb.sh
./scripts/package-appimage.sh

# Cross-compile Windows binary, ZIP, and NSIS installer
./scripts/package-windows.sh
./scripts/package-windows-installer.sh

# Generate and sign release manifest
./scripts/sign-release.sh "0.1.0"
```

---

## Documentation

Comprehensive design and stage documentation is available in `docs/`:

- [General Plan & Architecture](docs/general-plan.md)
- [Feature Parity Checklist](docs/feature-parity-checklist.md)
- [Stage 1: Player Prototype](docs/stage-01-player-prototype.md)
- [Stage 2: Auth & Browsing](docs/stage-02-auth-and-browsing.md)
- [Stage 3: Playback Parity](docs/stage-03-playback-parity.md)
- [Stage 4: Desktop UI & Features](docs/stage-04-desktop-ui-and-features.md)
- [Stage 5: Packaging & Release](docs/stage-05-packaging-and-release.md)

---

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

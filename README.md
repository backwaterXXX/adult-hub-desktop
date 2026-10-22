# Adult Hub Desktop

> A native desktop app for browsing Jellyfin libraries and playing Adult Hub Resolver streams.

Adult Hub Desktop connects to a Jellyfin server you configure. It brings library browsing, performer and studio details, Resolver channels, downloads, and video playback together in one Windows or Linux app.

## How It Works

1. Connect the app to your Jellyfin server with Quick Connect or your account credentials.
2. Browse your libraries, search titles, and explore performers, studios, and channels.
3. Play a video in the built-in libmpv player or add it to the download queue.

The app is a client. You provide the Jellyfin server and, when needed, a separate Resolver service.

## Key Features

- **Jellyfin library browsing**: Search titles, browse categories, and view performer and studio profiles.
- **Resolver channels**: Browse channel listings and play supported streams in the built-in player.
- **Integrated video player**: Use hardware-accelerated playback, subtitles, audio tracks, seeking, volume controls, and fullscreen mode.
- **Download queue**: Queue downloads, pause or resume work, and review completed items.
- **Playback progress**: Resume videos and report playback progress to Jellyfin.
- **Keyboard and HTPC controls**: Navigate the app without a mouse; use `Ctrl+Shift+Q` for the panic exit.
- **Signed updates**: Verify the release signature and package hash before installing an in-app update.

## Supported Platforms

- **Windows 10/11, 64-bit**: The app uses `adult-hub-desktop.exe` and `libmpv-2.dll` from the same folder. Video playback requires a graphics driver with the Vulkan Runtime.
- **Linux, 64-bit**: Install the Debian/Ubuntu `.deb` package or run the AppImage.

## Installation

Download the latest package from [GitHub Releases](https://github.com/backwaterXXX/adult-hub-desktop/releases/latest).

### Windows

Download both `adult-hub-desktop.exe` and `libmpv-2.dll` into one folder, then run the EXE.

### Linux

Use the `.deb` package on Debian or Ubuntu. For other distributions, download the AppImage and run:

```sh
chmod +x Adult_Hub_Desktop-x86_64.AppImage
./Adult_Hub_Desktop-x86_64.AppImage
```

## Quick Start

### 1. Connect to Jellyfin

Enter your Jellyfin server URL, then sign in with Quick Connect or your username and password. Use HTTPS for servers reached over the internet. HTTP is intended for local network connections and requires in-app consent.

### 2. Browse your library

Choose a library to browse. Use search and filters to find titles, performers, studios, or categories.

### 3. Configure Resolver if needed

The Resolver URL follows the Jellyfin server by default. If your Resolver is hosted separately, set its address in **Settings → Integrations & Resolver**.

## Privacy and Account Data

The app connects to the Jellyfin and Resolver addresses you configure and checks GitHub Releases for updates. It does not bundle a Jellyfin or Resolver server. Credentials and account settings are stored locally; the app uses Windows Credential Manager or Linux Secret Service for protected storage when available.

Use **Settings → Privacy** for the panic exit and related privacy controls. Diagnostics are exported only when you request an export, and sensitive values are redacted.

## Frequently Asked Questions

<details>
<summary><strong>Why does Windows need two files?</strong></summary>
<br>
The executable loads the adjacent `libmpv-2.dll` for video playback. Keep both files in the same folder.
</details>

<details>
<summary><strong>Can I connect to a local Jellyfin server over HTTP?</strong></summary>
<br>
Yes. The app supports HTTP for local network addresses after you approve the in-app consent prompt. Use HTTPS for public or remote servers.
</details>

<details>
<summary><strong>Where are my credentials stored?</strong></summary>
<br>
Credentials are kept on the device. The app uses the platform's protected credential store when available; if secure storage is unavailable, saved credentials are not silently written in plaintext.
</details>

## Building from Source

The Cargo workspace and Rust toolchain configuration are under `desktop/`. A Docker-based development environment is available in `dev/`.

```sh
# Start an interactive development shell
./dev/run.sh

# Build the workspace
./dev/run.sh cargo build --manifest-path desktop/Cargo.toml

# Check formatting
./dev/run.sh cargo fmt --manifest-path desktop/Cargo.toml --all -- --check
```

Packaging scripts are available in `scripts/`:

```sh
./scripts/package-deb.sh
./scripts/package-appimage.sh
./scripts/package-windows.sh
```

### Workspace Crates

| Crate | Purpose |
|---|---|
| `adult-hub-app` | Desktop application, UI, and update flow |
| `adult-hub-client` | Jellyfin and Resolver API clients |
| `adult-hub-player` | libmpv playback integration |
| `adult-hub-storage` | Local database, settings, and account storage |

## License and Notices

Licensed under the [Apache License 2.0](LICENSE). Third-party licenses and notices are listed in [NOTICES.txt](NOTICES.txt).

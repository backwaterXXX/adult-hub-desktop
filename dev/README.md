# Development container

Pinned Linux build environment for Adult Hub Desktop. The project does not
build on the host directly; all Rust and native work runs inside this image so
the toolchain is reproducible and matches the target Ubuntu 24.04 baseline.

## What it contains

| Category | Versions / packages |
| --- | --- |
| Base | Ubuntu 24.04 LTS |
| Rust | 1.99.1 (rustup, `rustfmt` and `clippy` included) |
| C/C++ toolchain | `build-essential`, `clang`, `lld`, `cmake`, `ninja-build`, `pkg-config`, `nasm` |
| Playback | `libmpv-dev`, `mpv`, `ffmpeg` |
| UI | Slint dependencies: X11, Wayland, `libxkbcommon`, fontconfig, FreeType, OpenGL/EGL |
| Data and crypto | `libsqlite3-dev`, `libssl-dev` |
| Packaging | `patchelf`, `fakeroot`, `dpkg-deb`, `desktop-file-utils`, `file` |
| Verification | `xvfb`, `xauth`, `mesa-utils` (software rendering fallback) |

The image defines a `dev` user with the same uid/gid as the host account
(1000 by default), so bind-mounted sources and build output are never owned by
root.

## Build

```sh
docker build -t adult-hub-desktop-dev:rust-1.99.1 \
  --build-arg USER_ID="$(id -u)" \
  --build-arg GROUP_ID="$(id -g)" \
  dev
```

Rebuild when `dev/Dockerfile` changes and bump the image tag when the Rust or
native dependency versions change; record the change in the release
dependencies.

## Run

```sh
dev/run.sh                    # interactive shell in /work
dev/run.sh cargo --version    # single command
```

`dev/run.sh` mounts:

- the repository at `/work`,
- `/tmp/.X11-unix` and the current `DISPLAY` for X11 window tests,
- `/dev/dri` for VA-API hardware decoding,
- a named volume `ah-desktop-cargo` for the Cargo registry cache.

## Platform limits

This container covers Linux x64 development and X11 checks. It does not
replace the Stage 1 acceptance evidence that the plan requires:

- **Windows 10/11** must be validated on the dedicated Windows machine or in
  CI (`windows-latest`). Cross-compiling alone is not acceptance evidence.
- **Wayland** needs a real compositor session; the container can only reach
  the host display socket. Run Wayland validation in a Wayland session.
- **Hardware decode and HDR-to-SDR** need physical GPU/driver coverage on
  Intel, AMD and NVIDIA. Software rendering (llvmpipe) is functional only.

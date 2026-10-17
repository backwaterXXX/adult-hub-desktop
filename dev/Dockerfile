# Development image for Adult Hub Desktop.
#
# Provides the pinned Rust toolchain plus native build dependencies required by
# Stages 1-5 of the plan: Slint on Linux (X11 and Wayland), libmpv, FFmpeg,
# SQLite, OpenSSL and packaging tools.
#
# Build:  docker build -t adult-hub-desktop-dev:rust-1.99.1 dev
# Run:    dev/run.sh
FROM ubuntu:24.04

ARG RUST_VERSION=1.99.1
ARG RUSTUP_VERSION=1.28.2

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        ninja-build \
        pkg-config \
        nasm \
        clang \
        lld \
        libmpv-dev \
        mpv \
        ffmpeg \
        libsqlite3-dev \
        libssl-dev \
        libxkbcommon-dev \
        libxkbcommon-x11-dev \
        libfontconfig1-dev \
        libfreetype-dev \
        libgl1-mesa-dev \
        libegl1-mesa-dev \
        libgles2-mesa-dev \
        libx11-dev \
        libxcursor-dev \
        libxrandr-dev \
        libxi-dev \
        libxinerama-dev \
        libwayland-dev \
        wayland-protocols \
        libasound2-dev \
        libpulse-dev \
        libdbus-1-dev \
        patchelf \
        fakeroot \
        file \
        desktop-file-utils \
        ca-certificates \
        curl \
        git \
        jq \
        unzip \
        xz-utils \
        xvfb \
        xauth \
        mesa-utils \
    && rm -rf /var/lib/apt/lists/*

# Install the pinned Rust toolchain for all users. The image is meant to run as
# the host user's uid so bind-mounted sources are never owned by root.
RUN curl --proto '=https' --tlsv1.2 -sSf \
        https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --no-modify-path --profile default \
        --default-toolchain "${RUST_VERSION}" \
    && rm /tmp/rustup-init.sh \
    && chmod -R a+rX "${RUSTUP_HOME}" "${CARGO_HOME}" \
    && rustc --version \
    && cargo --version \
    && rustup component add rustfmt clippy

# Match the host developer's uid/gid when building with:
#   docker build --build-arg USER_ID=$(id -u) --build-arg GROUP_ID=$(id -g) ...
ARG USER_ID=1000
ARG GROUP_ID=1000

# Ubuntu 24.04 ships a default "ubuntu" user with uid 1000 which would collide
# with the build-time uid argument; remove it before creating the dev user.
RUN userdel --remove ubuntu 2>/dev/null || true \
    && groupdel ubuntu 2>/dev/null || true \
    && groupadd --gid "${GROUP_ID}" dev \
    && useradd --uid "${USER_ID}" --gid "${GROUP_ID}" --create-home --shell /bin/bash dev \
    && mkdir -p /home/dev/.cargo /work \
    && chown -R dev:dev /home/dev /work

USER dev:dev
WORKDIR /work

CMD ["bash"]

#!/usr/bin/env bash
# Run the Adult Hub Desktop development container.
#
# The project root is bind-mounted at /work with the host user's uid/gid, so
# build artifacts created inside the container stay editable on the host.
# The X11 socket and DRM render node are passed through when available, which
# lets the Stage 1 player prototype open a real window and use hardware decode.
#
# Usage:
#   dev/run.sh                  # interactive shell
#   dev/run.sh cargo build      # run a single command
set -euo pipefail

IMAGE="${AH_DEV_IMAGE:-adult-hub-desktop-dev:rust-1.99.1}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! docker image inspect "${IMAGE}" >/dev/null 2>&1; then
    echo "Image ${IMAGE} not found. Build it first:" >&2
    echo "  docker build -t ${IMAGE} \\" >&2
    echo "    --build-arg USER_ID=$(id -u) --build-arg GROUP_ID=$(id -g) \"${ROOT_DIR}/dev\"" >&2
    exit 1
fi

docker_args=(
    --rm
    --interactive
    --tty
    --volume "${ROOT_DIR}:/work"
    --volume ah-desktop-cargo:/home/dev/.cargo
    --workdir /work
    --env HOME=/home/dev
    --env CARGO_HOME=/home/dev/.cargo
    --env RUST_BACKTRACE=1
)

# Reuse host X11 display when present.
if [[ -n "${DISPLAY:-}" && -d /tmp/.X11-unix ]]; then
    docker_args+=(--volume /tmp/.X11-unix:/tmp/.X11-unix:ro --env "DISPLAY=${DISPLAY}")
    if [[ -f "${XAUTHORITY:-$HOME/.Xauthority}" ]]; then
        docker_args+=(--volume "${XAUTHORITY:-$HOME/.Xauthority}:/tmp/.Xauthority:ro" --env XAUTHORITY=/tmp/.Xauthority)
    fi
fi

# Pass through the DRM render node for VA-API hardware decoding.
if [[ -d /dev/dri ]]; then
    docker_args+=(--device /dev/dri)
fi

exec docker run "${docker_args[@]}" "${IMAGE}" "${@:-bash}"

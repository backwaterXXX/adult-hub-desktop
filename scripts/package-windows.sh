#!/usr/bin/env bash
# Build the Windows x64 executable and its adjacent libmpv runtime in bin/.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
CACHE_DIR="${ROOT_DIR}/.cache/windows"
TARGET="x86_64-pc-windows-gnu"
MPV_VER="20261011-git-accb7b2821"
MPV_URL="https://sourceforge.net/projects/mpv-player-windows/files/libmpv/mpv-dev-x86_64-${MPV_VER}.7z/download"
MPV_DLL="${CACHE_DIR}/libmpv-2.dll"
EXE_PATH="${ROOT_DIR}/desktop/target/${TARGET}/release/adult-hub-desktop.exe"

# 1. Automatically increment patch version once per application build
# Clean up re-entrancy marker on exit so future runs in the same shell still bump
trap 'unset ADULT_HUB_VERSION_BUMPED 2>/dev/null || true' EXIT
if [[ "${SKIP_VERSION_BUMP:-0}" != "1" && "${ADULT_HUB_VERSION_BUMPED:-0}" != "1" && "${CI:-false}" != "true" && "${GITHUB_ACTIONS:-false}" != "true" ]]; then
    "${ROOT_DIR}/scripts/bump-version.sh"
    export ADULT_HUB_VERSION_BUMPED=1
fi

VERSION=$(grep -m 1 '^version = ' "${ROOT_DIR}/desktop/Cargo.toml" | sed -E 's/version = "([^"]+)"/\1/')
COMMIT_HASH="$(git rev-parse --short=8 HEAD 2>/dev/null || true)"
if [[ -z "${COMMIT_HASH}" || "${COMMIT_HASH}" == "unknown" ]]; then
    if [[ -f "${ROOT_DIR}/.git/HEAD" ]]; then
        head_ref=$(cat "${ROOT_DIR}/.git/HEAD")
        if [[ "${head_ref}" =~ ^ref:\ (.*) ]]; then
            ref_file="${ROOT_DIR}/.git/${BASH_REMATCH[1]}"
            if [[ -f "${ref_file}" ]]; then
                COMMIT_HASH=$(cut -c1-8 "${ref_file}")
            fi
        else
            COMMIT_HASH=$(echo "${head_ref}" | cut -c1-8)
        fi
    fi
fi
export ADULT_HUB_COMMIT_HASH="${COMMIT_HASH:-}"
export ADULT_HUB_BUILD_DATE="$(date -u +%Y-%m-%d)"

echo "==> Preparing Windows build for Adult Hub Desktop v${VERSION} (${ADULT_HUB_COMMIT_HASH}, ${ADULT_HUB_BUILD_DATE})..."

mkdir -p "${CACHE_DIR}" "${BIN_DIR}"
if [[ ! -s "${MPV_DLL}" ]]; then
    echo "==> Downloading the pinned libmpv runtime..."
    archive="${CACHE_DIR}/mpv-dev.7z"
    curl --fail --location --silent --show-error "${MPV_URL}" -o "${archive}"
    7z e "${archive}" libmpv-2.dll -o"${CACHE_DIR}" -y
    rm -f "${archive}"
fi

"${ROOT_DIR}/scripts/warn-target-size.sh"

echo "==> Building the Windows executable (${TARGET})..."
if [[ "${AH_DIRECT_BUILD:-0}" == "1" ]]; then
    # Some agent shells wrap cargo and try to write launch state under the
    # user's read-only home directory. Resolve the real toolchain binaries
    # through rustup and call cargo directly in that case.
    CARGO_BIN="$(rustup which cargo)"
    RUSTC_BIN="$(rustup which rustc)"
    CFLAGS_x86_64_pc_windows_gnu="${CFLAGS_x86_64_pc_windows_gnu:--O2}" \
    RUSTC="${RUSTC_BIN}" \
    "${CARGO_BIN}" build --manifest-path desktop/Cargo.toml --target "${TARGET}" --release
    echo "==> Stripping debug symbols..."
    if command -v x86_64-w64-mingw32-strip >/dev/null 2>&1; then
        x86_64-w64-mingw32-strip "${EXE_PATH}"
    fi
elif command -v docker >/dev/null 2>&1 && docker image inspect "${AH_DEV_IMAGE:-adult-hub-desktop-dev:rust-1.99.1}" >/dev/null 2>&1; then
    "${ROOT_DIR}/dev/run.sh" cargo build --manifest-path desktop/Cargo.toml --target "${TARGET}" --release
    echo "==> Stripping debug symbols..."
    "${ROOT_DIR}/dev/run.sh" x86_64-w64-mingw32-strip "desktop/target/${TARGET}/release/adult-hub-desktop.exe"
else
    CFLAGS_x86_64_pc_windows_gnu="${CFLAGS_x86_64_pc_windows_gnu:--O2}" \
    cargo build --manifest-path desktop/Cargo.toml --target "${TARGET}" --release
    echo "==> Stripping debug symbols..."
    if command -v x86_64-w64-mingw32-strip >/dev/null 2>&1; then
        x86_64-w64-mingw32-strip "${EXE_PATH}"
    fi
fi

"${ROOT_DIR}/scripts/warn-target-size.sh"

if [[ ! -f "${EXE_PATH}" ]]; then
    echo "Error: Expected build output ${EXE_PATH} does not exist!" >&2
    exit 1
fi
mkdir -p "${BIN_DIR}"
rm -f "${BIN_DIR}/adult-hub-desktop.exe" 2>/dev/null || true
cp -f "${EXE_PATH}" "${BIN_DIR}/adult-hub-desktop.exe"
cp -f "${MPV_DLL}" "${BIN_DIR}/libmpv-2.dll"
if ! cmp -s "${EXE_PATH}" "${BIN_DIR}/adult-hub-desktop.exe"; then
    echo "Error: Copy failed or ${BIN_DIR}/adult-hub-desktop.exe is out of sync with ${EXE_PATH}!" >&2
    exit 1
fi

echo ""
echo "============================================================"
echo "Adult Hub Desktop Windows Build Complete"
echo "Version:         ${VERSION}"
echo "Commit:          ${ADULT_HUB_COMMIT_HASH}"
echo "Build Date:      ${ADULT_HUB_BUILD_DATE}"
echo "Target:          ${TARGET}"
echo "Output Directory: ${BIN_DIR}"
echo "Executable:      ${BIN_DIR}/adult-hub-desktop.exe"
echo "Runtime DLL:     ${BIN_DIR}/libmpv-2.dll"
echo "------------------------------------------------------------"
file "${BIN_DIR}/adult-hub-desktop.exe"
file "${BIN_DIR}/libmpv-2.dll"
du -h "${BIN_DIR}/adult-hub-desktop.exe" "${BIN_DIR}/libmpv-2.dll"
sha256sum "${BIN_DIR}/adult-hub-desktop.exe"
sha256sum "${BIN_DIR}/libmpv-2.dll"
echo "============================================================"

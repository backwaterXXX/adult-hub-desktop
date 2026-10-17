#!/usr/bin/env bash
# Package Adult Hub Desktop as an x86_64 Linux AppImage
# Always outputs to bin/Adult_Hub_Desktop-x86_64.AppImage
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
APPDIR="${ROOT_DIR}/dist/AppDir"
CACHE_DIR="${ROOT_DIR}/.cache/appimage"
APPIMAGETOOL="${CACHE_DIR}/appimagetool"
OUTPUT_NAME="Adult_Hub_Desktop-x86_64.AppImage"

mkdir -p "${BIN_DIR}" "${CACHE_DIR}"

if [[ ! -f "${BIN_DIR}/adult-hub-desktop" ]]; then
    echo "==> Linux binary missing, building release..."
    "${ROOT_DIR}/scripts/build-linux.sh" release
fi

if [[ ! -f "${APPIMAGETOOL}" ]]; then
    echo "==> Downloading appimagetool..."
    curl -sL "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage" -o "${APPIMAGETOOL}"
    chmod +x "${APPIMAGETOOL}"
fi

echo "==> Preparing AppDir in ${APPDIR}..."
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/lib"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/scalable/apps"

# Copy binary
cp "${BIN_DIR}/adult-hub-desktop" "${APPDIR}/usr/bin/adult-hub-desktop"
chmod 755 "${APPDIR}/usr/bin/adult-hub-desktop"

# Desktop integration files
cp "${ROOT_DIR}/desktop/adult-hub-desktop.desktop" "${APPDIR}/"
cp "${ROOT_DIR}/desktop/adult-hub-desktop.desktop" "${APPDIR}/usr/share/applications/"
cp "${ROOT_DIR}/desktop/assets/adult-hub-desktop.png" "${APPDIR}/"
cp "${ROOT_DIR}/desktop/assets/adult-hub-desktop.png" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/"
cp "${ROOT_DIR}/desktop/assets/adult-hub-desktop.svg" "${APPDIR}/"
cp "${ROOT_DIR}/desktop/assets/adult-hub-desktop.svg" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/"

# Copy license and notices
cp "${ROOT_DIR}/LICENSE" "${APPDIR}/"
cp "${ROOT_DIR}/NOTICES.txt" "${APPDIR}/"

# AppRun script
cat << 'EOF' > "${APPDIR}/AppRun"
#!/bin/sh
SELF=$(readlink -f "$0")
HERE=${SELF%/*}
export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib:${HERE}/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH}"
export APPDIR="${HERE}"
exec "${HERE}/usr/bin/adult-hub-desktop" "$@"
EOF
chmod +x "${APPDIR}/AppRun"

echo "==> Building AppImage with appimagetool..."
ARCH=x86_64 "${APPIMAGETOOL}" --appimage-extract-and-run "${APPDIR}" "${BIN_DIR}/${OUTPUT_NAME}"

rm -rf "${APPDIR}"

echo "==> AppImage created successfully: ${BIN_DIR}/${OUTPUT_NAME}"
ls -lh "${BIN_DIR}/${OUTPUT_NAME}"

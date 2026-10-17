#!/usr/bin/env bash
# Package Adult Hub Desktop as a Debian / Ubuntu .deb package
# Always outputs to bin/adult-hub-desktop_0.1.0_amd64.deb
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
STAGING_DIR="${ROOT_DIR}/dist/deb-staging"
VERSION="0.1.0"
DEB_NAME="adult-hub-desktop_${VERSION}_amd64.deb"

mkdir -p "${BIN_DIR}"

if [[ ! -f "${BIN_DIR}/adult-hub-desktop" ]]; then
    echo "==> Linux binary missing, building release..."
    "${ROOT_DIR}/scripts/build-linux.sh" release
fi

echo "==> Preparing .deb package structure in ${STAGING_DIR}..."
rm -rf "${STAGING_DIR}"
mkdir -p "${STAGING_DIR}/DEBIAN"
mkdir -p "${STAGING_DIR}/usr/bin"
mkdir -p "${STAGING_DIR}/usr/share/applications"
mkdir -p "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps"
mkdir -p "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${STAGING_DIR}/usr/share/doc/adult-hub-desktop"

# Binary
cp "${BIN_DIR}/adult-hub-desktop" "${STAGING_DIR}/usr/bin/adult-hub-desktop"
chmod 755 "${STAGING_DIR}/usr/bin/adult-hub-desktop"

# Desktop file
cp "${ROOT_DIR}/desktop/adult-hub-desktop.desktop" "${STAGING_DIR}/usr/share/applications/"
chmod 644 "${STAGING_DIR}/usr/share/applications/adult-hub-desktop.desktop"

# Icons
cp "${ROOT_DIR}/desktop/assets/adult-hub-desktop.svg" "${STAGING_DIR}/usr/share/icons/hicolor/scalable/apps/"
cp "${ROOT_DIR}/desktop/assets/adult-hub-desktop.png" "${STAGING_DIR}/usr/share/icons/hicolor/256x256/apps/"

# Copyright & Notices
cat "${ROOT_DIR}/LICENSE" > "${STAGING_DIR}/usr/share/doc/adult-hub-desktop/copyright"
echo -e "\n\n" >> "${STAGING_DIR}/usr/share/doc/adult-hub-desktop/copyright"
cat "${ROOT_DIR}/NOTICES.txt" >> "${STAGING_DIR}/usr/share/doc/adult-hub-desktop/copyright"

# Control file
cat << EOF > "${STAGING_DIR}/DEBIAN/control"
Package: adult-hub-desktop
Version: ${VERSION}
Section: video
Priority: optional
Architecture: amd64
Depends: libc6 (>= 2.34), libmpv2 | libmpv1, libgl1, libasound2, libx11-6
Maintainer: Adult Hub Team <dev@adult-hub>
Homepage: https://github.com/adult-hub/adult-hub-desktop
Description: Adult Hub Desktop
 Cross-platform media player and catalog browser for adult content.
 Features hardware accelerated playback via libmpv, visual parity with
 Android TV, privacy panic shutdown, and cloud pHash video fingerprinting.
EOF

echo "==> Building .deb package..."
dpkg-deb --build --root-owner-group "${STAGING_DIR}" "${BIN_DIR}/${DEB_NAME}"

rm -rf "${STAGING_DIR}"

echo "==> .deb package created: ${BIN_DIR}/${DEB_NAME}"
ls -lh "${BIN_DIR}/${DEB_NAME}"

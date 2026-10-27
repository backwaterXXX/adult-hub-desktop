#!/usr/bin/env bash
# Bump patch version in desktop/Cargo.toml
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TOML="${ROOT_DIR}/desktop/Cargo.toml"
CARGO_LOCK="${ROOT_DIR}/desktop/Cargo.lock"

if [[ ! -f "${CARGO_TOML}" ]]; then
    echo "Error: ${CARGO_TOML} not found" >&2
    exit 1
fi

# Re-entrancy protection for nested script invocations within the same run
if [[ "${ADULT_HUB_VERSION_BUMPED:-0}" == "1" ]]; then
    CURRENT_VERSION=$(grep -m 1 '^version = ' "${CARGO_TOML}" | sed -E 's/version = "([^"]+)"/\1/')
    echo "Version already bumped in this run (${CURRENT_VERSION})"
    exit 0
fi

# Extract current version from [workspace.package]
CURRENT_VERSION=$(grep -m 1 '^version = ' "${CARGO_TOML}" | sed -E 's/version = "([^"]+)"/\1/')

if [[ -z "${CURRENT_VERSION}" ]]; then
    echo "Error: could not find version in ${CARGO_TOML}" >&2
    exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "${CURRENT_VERSION}"
NEW_PATCH=$((PATCH + 1))
NEW_VERSION="${MAJOR}.${MINOR}.${NEW_PATCH}"

# Replace version in desktop/Cargo.toml and desktop/Cargo.lock
if command -v python3 >/dev/null 2>&1; then
    python3 -c "
import re
from pathlib import Path

toml_path = Path('${CARGO_TOML}')
content = toml_path.read_text(encoding='utf-8')
new_content, count = re.subn(r'(?m)^(\s*version\s*=\s*\")[0-9]+\.[0-9]+\.[0-9]+(\")', r'\g<1>${NEW_VERSION}\2', content, count=1)
if count == 0:
    raise SystemExit('Version pattern not found in ${CARGO_TOML}')
toml_path.write_text(new_content, encoding='utf-8')

lock_path = Path('${CARGO_LOCK}')
if lock_path.exists():
    lock_content = lock_path.read_text(encoding='utf-8')
    for pkg in ['adult-hub-desktop', 'adult-hub-client', 'adult-hub-player', 'adult-hub-storage']:
        pattern = rf'(\[\[package\]\]\nname = \"{pkg}\"\nversion = \")[0-9]+\.[0-9]+\.[0-9]+(\")'
        lock_content = re.sub(pattern, rf'\g<1>${NEW_VERSION}\g<2>', lock_content)
    lock_path.write_text(lock_content, encoding='utf-8')
"
else
    sed -i "0,/^version = \"${CURRENT_VERSION}\"/s//version = \"${NEW_VERSION}\"/" "${CARGO_TOML}"
    if [[ -f "${CARGO_LOCK}" ]]; then
        sed -i "s/version = \"${CURRENT_VERSION}\"/version = \"${NEW_VERSION}\"/g" "${CARGO_LOCK}"
    fi
fi

export ADULT_HUB_VERSION_BUMPED=1
echo "Bumped version from ${CURRENT_VERSION} to ${NEW_VERSION} (Windows resource: ${MAJOR}.${MINOR}.${NEW_PATCH}.0)"

#!/usr/bin/env bash
# Bump patch version in desktop/Cargo.toml
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TOML="${ROOT_DIR}/desktop/Cargo.toml"

if [[ ! -f "${CARGO_TOML}" ]]; then
    echo "Error: ${CARGO_TOML} not found" >&2
    exit 1
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

# Replace version in desktop/Cargo.toml
sed -i "0,/^version = \"${CURRENT_VERSION}\"/s//version = \"${NEW_VERSION}\"/" "${CARGO_TOML}"

echo "Bumped version from ${CURRENT_VERSION} to ${NEW_VERSION}"

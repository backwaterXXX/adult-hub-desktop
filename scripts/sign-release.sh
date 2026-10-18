#!/usr/bin/env bash
# Generate and sign adult-hub-update-manifest.json with Ed25519
# Outputs:
#   bin/adult-hub-update-manifest.json
#   bin/adult-hub-update-manifest.json.sig
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/bin"
VERSION="${1:-0.1.0}"
MANIFEST_FILE="${BIN_DIR}/adult-hub-update-manifest.json"
SIG_FILE="${BIN_DIR}/adult-hub-update-manifest.json.sig"
KEY_FILE="${ROOT_DIR}/dev/release-signing-key.pem"

if [[ ! -d "${BIN_DIR}" ]]; then
    echo "Error: ${BIN_DIR} directory does not exist!" >&2
    exit 1
fi

echo "==> Generating release update manifest for v${VERSION}..."

python3 - << EOF
import os, json, hashlib, subprocess

def valid_repository(value):
    parts = value.split("/")
    return (
        len(parts) == 2
        and all(
            part
            and all(c.isascii() and (c.isalnum() or c in "-_.") for c in part)
            for part in parts
        )
    )

repository = os.environ.get("GITHUB_REPOSITORY", "")
if not valid_repository(repository):
    remote = subprocess.run(
        ["git", "remote", "get-url", "origin"],
        check=False,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if "github.com" in remote:
        candidate = remote.split("github.com", 1)[1].lstrip(":/").removesuffix(".git").rstrip("/")
        repository = candidate if valid_repository(candidate) else ""
if not valid_repository(repository):
    repository = "adult-hub/adult-hub-desktop"

bin_dir = "${BIN_DIR}"
version = "${VERSION}"

packages = []

files_map = [
    ("windows", "x86_64", "exe", "adult-hub-desktop.exe"),
    ("linux", "x86_64", "deb", f"adult-hub-desktop_{version}_amd64.deb"),
    ("linux", "x86_64", "appimage", "Adult_Hub_Desktop-x86_64.AppImage"),
]

for platform, arch, pkg_type, filename in files_map:
    path = os.path.join(bin_dir, filename)
    if os.path.isfile(path):
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
        package = {
            "platform": platform,
            "arch": arch,
            "package_type": pkg_type,
            "url": f"https://github.com/{repository}/releases/download/desktop-v{version}/{filename}",
            "sha256": digest,
            "size_bytes": size,
        }
        if platform == "windows":
            runtime_path = os.path.join(bin_dir, "libmpv-2.dll")
            if not os.path.isfile(runtime_path):
                raise SystemExit(f"Windows runtime DLL is missing: {runtime_path}")
            with open(runtime_path, "rb") as runtime_file:
                runtime_bytes = runtime_file.read()
            package["runtime_dll"] = {
                "platform": "windows",
                "arch": "x86_64",
                "package_type": "runtime-dll",
                "url": f"https://github.com/{repository}/releases/download/desktop-v{version}/libmpv-2.dll",
                "sha256": hashlib.sha256(runtime_bytes).hexdigest(),
                "size_bytes": len(runtime_bytes),
            }
            print(f"  [+] libmpv-2.dll: {len(runtime_bytes)} bytes, sha256={package['runtime_dll']['sha256']}")
        packages.append(package)
        print(f"  [+] {filename}: {size} bytes, sha256={digest}")

manifest = {
    "schema_version": 1,
    "version": version,
    "release_notes_url": f"https://github.com/{repository}/releases/tag/desktop-v{version}",
    "release_notes": f"Adult Hub Desktop Release v{version} — Windows executable with adjacent libmpv runtime DLL, OLED theme, and privacy panic exit.",
    "packages": packages,
}

manifest_path = "${MANIFEST_FILE}"
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2)

print(f"==> Manifest written to: {manifest_path}")
EOF

echo "==> Signing manifest with Ed25519..."
"${ROOT_DIR}/dev/run.sh" openssl pkeyutl -sign \
    -inkey "/work/dev/release-signing-key.pem" \
    -rawin -in "/work/bin/adult-hub-update-manifest.json" \
    -out "/work/bin/adult-hub-update-manifest.json.sig"

echo "==> Detached signature written to: ${SIG_FILE}"
ls -lh "${MANIFEST_FILE}" "${SIG_FILE}"

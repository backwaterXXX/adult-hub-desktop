#!/usr/bin/env bash
# Warn when generated Rust build artifacts exceed 20 GiB.
target_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/desktop/target"
if [[ -d "$target_dir" ]]; then
    target_kib=$(du -sk "$target_dir" 2>/dev/null | cut -f1) || exit 0
    if [[ "$target_kib" =~ ^[0-9]+$ ]] && (( target_kib > 20 * 1024 * 1024 )); then
        echo "Warning: $target_dir uses $(du -sh "$target_dir" | cut -f1) (over 20 GiB)." >&2
        echo "To remove generated build artifacts after builds finish: rm -rf -- \"$target_dir\"" >&2
    fi
fi

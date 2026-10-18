#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

rm -f /tmp/ah_demo_step_*

# Start Xvfb
Xvfb :99 -screen 0 1180x790x24 &
XVFB_PID=$!
sleep 2

cleanup() {
    kill "${XVFB_PID}" 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p docs/screenshots

DISPLAY=:99 desktop/target/debug/adult-hub-desktop --demo-capture &
APP_PID=$!

wait_for_step() {
    local step_file="$1"
    local name="$2"
    local count=0
    while [[ ! -f "${step_file}" ]]; do
        sleep 0.2
        count=$((count + 1))
        if [[ $count -gt 100 ]]; then
            echo "Timeout waiting for ${step_file}" >&2
            return 1
        fi
    done
    sleep 0.3
    ffmpeg -y -f x11grab -video_size 1180x790 -i :99.0 -vframes 1 "${name}" >/dev/null 2>&1
    echo "Captured ${name}"
}

wait_for_step "/tmp/ah_demo_step_1" "docs/screenshots/01_home_featured.png"
wait_for_step "/tmp/ah_demo_step_2" "docs/screenshots/02_browse.png"
wait_for_step "/tmp/ah_demo_step_3" "docs/screenshots/03_channels.png"
wait_for_step "/tmp/ah_demo_step_4" "docs/screenshots/06_player_empty.png"
wait_for_step "/tmp/ah_demo_step_5" "docs/screenshots/04_player_playing_controls.png"
wait_for_step "/tmp/ah_demo_step_6" "docs/screenshots/05_player_playing_clean.png"
wait_for_step "/tmp/ah_demo_step_7" "docs/screenshots/07_settings.png"

# Wait for process exit
wait "${APP_PID}" 2>/dev/null || true

# Generate close up of card with long title from 01_home_featured.png
ffmpeg -y -i docs/screenshots/01_home_featured.png -vf "crop=220:140:465:440" docs/screenshots/08_card_long_title_closeup.png >/dev/null 2>&1
echo "Captured docs/screenshots/08_card_long_title_closeup.png"

echo "All screenshots captured successfully!"

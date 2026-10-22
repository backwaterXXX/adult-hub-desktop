#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe

mkdir -p "${ROOT_DIR}/docs/screenshots"

# Ensure clean state
rm -f /tmp/ah_demo_step_*
killall -9 Xvfb 2>/dev/null || true
sleep 1

# ==========================================
# PASS 1: Standard Desktop Resolution (1180x790)
# ==========================================
echo "==> Starting Pass 1: 1180x790 Desktop Mode..."
Xvfb :99 -screen 0 1180x790x24 &
XVFB_PID=$!
sleep 2

cleanup() {
    kill "${XVFB_PID:-}" 2>/dev/null || true
    kill "${XVFB_PID2:-}" 2>/dev/null || true
    killall -9 Xvfb 2>/dev/null || true
}
trap cleanup EXIT

DISPLAY=:99 AH_WINDOW_SIZE=1180x790 "${ROOT_DIR}/desktop/target/debug/adult-hub-desktop" --demo-capture &
APP_PID=$!

wait_for_step() {
    local step_file="$1"
    local name="$2"
    local disp="${3:-:99}"
    local count=0
    while [[ ! -f "${step_file}" ]]; do
        sleep 0.2
        count=$((count + 1))
        if [[ $count -gt 150 ]]; then
            echo "Timeout waiting for ${step_file}" >&2
            return 1
        fi
    done
    sleep 0.4
    xwd -root -display "${disp}" -out /tmp/screen.xwd
    ffmpeg -y -i /tmp/screen.xwd "${ROOT_DIR}/${name}" >/dev/null 2>&1
    echo "Captured ${name}"
}

wait_for_step "/tmp/ah_demo_step_1" "docs/screenshots/01_home_featured.png"
wait_for_step "/tmp/ah_demo_step_1a" "docs/screenshots/01a_home_performers_shelf.png"
wait_for_step "/tmp/ah_demo_step_2" "docs/screenshots/02_browse_expanded.png"
wait_for_step "/tmp/ah_demo_step_2a" "docs/screenshots/02a_browse_performers.png"
wait_for_step "/tmp/ah_demo_step_2b" "docs/screenshots/02b_browse_studios.png"
wait_for_step "/tmp/ah_demo_step_3" "docs/screenshots/03_item_details_fullscreen.png"
wait_for_step "/tmp/ah_demo_step_3a" "docs/screenshots/03_item_details_scroll.png"
wait_for_step "/tmp/ah_demo_step_4" "docs/screenshots/04_performer_sage_roux_collapsed.png"
wait_for_step "/tmp/ah_demo_step_4a" "docs/screenshots/04a_performer_sage_roux_expanded.png"
wait_for_step "/tmp/ah_demo_step_4b" "docs/screenshots/04b_performer_tony_genius_loading.png"
wait_for_step "/tmp/ah_demo_step_4c" "docs/screenshots/04c_performer_tony_genius_loaded.png"
wait_for_step "/tmp/ah_demo_step_5" "docs/screenshots/05_studio_profile_fullscreen.png"
wait_for_step "/tmp/ah_demo_step_6" "docs/screenshots/06_settings_categories_chips.png"
wait_for_step "/tmp/ah_demo_step_7" "docs/screenshots/07_player_loading_animated_spinner.png"

# ==========================================
# Volume Animation Burst Capture
# ==========================================
echo "==> Capturing Volume Animation Burst Frames..."
mkdir -p /tmp/vol_frames /tmp/vol_crops
rm -f /tmp/vol_frames/* /tmp/vol_crops/*

# Wait for closed state
count=0
while [[ ! -f "/tmp/ah_demo_step_8_closed" ]]; do
    sleep 0.1
    count=$((count + 1))
    if [[ $count -gt 150 ]]; then echo "Timeout waiting for step 8 closed" >&2; exit 1; fi
done
sleep 0.2
xwd -root -display :99 -out /tmp/vol_frames/frame_000.xwd

# Trigger opening animation and capture burst during 200ms opening
touch /tmp/ah_demo_step_8_start_open
for i in $(seq -w 1 14); do
    xwd -root -display :99 -out "/tmp/vol_frames/frame_0${i}.xwd" 2>/dev/null
    sleep 0.015
done

wait_for_step "/tmp/ah_demo_step_8" "docs/screenshots/08_player_volume_flyout.png"
xwd -root -display :99 -out /tmp/vol_frames/frame_015.xwd

# Trigger closing animation and capture burst during 200ms closing
touch /tmp/ah_demo_step_8_start_close
for i in $(seq -w 16 28); do
    xwd -root -display :99 -out "/tmp/vol_frames/frame_0${i}.xwd" 2>/dev/null
    sleep 0.015
done

count=0
while [[ ! -f "/tmp/ah_demo_step_8_closed_again" ]]; do
    sleep 0.1
    count=$((count + 1))
    if [[ $count -gt 100 ]]; then break; fi
done
xwd -root -display :99 -out /tmp/vol_frames/frame_029.xwd

wait_for_step "/tmp/ah_demo_step_8a" "docs/screenshots/08a_player_more_menu.png"
wait_for_step "/tmp/ah_demo_step_8b" "docs/screenshots/08b_player_stats_during_pause.png"
wait_for_step "/tmp/ah_demo_step_8c" "docs/screenshots/08c_player_controls_bar.png"
wait_for_step "/tmp/ah_demo_step_9" "docs/screenshots/09_player_fullscreen.png"

# Wait for process exit
wait "${APP_PID}" 2>/dev/null || true

# Process volume frames after app has safely exited
echo "==> Processing volume animation frames..."
crop_filter="crop=100:250:710:520"
c_idx=1
for f in $(ls -1 /tmp/vol_frames/frame_*.xwd | sort); do
    out_png=$(printf "/tmp/vol_crops/crop_%03d.png" "$c_idx")
    ffmpeg -y -i "$f" -vf "${crop_filter}" "${out_png}" >/dev/null 2>&1 || true
    c_idx=$((c_idx + 1))
done

# Generate video and animated gif
ffmpeg -y -framerate 14 -i "/tmp/vol_crops/crop_%03d.png" -c:v libx264 -pix_fmt yuv420p "${ROOT_DIR}/docs/screenshots/volume_animation.mp4" >/dev/null 2>&1 || true
ffmpeg -y -framerate 12 -i "/tmp/vol_crops/crop_%03d.png" "${ROOT_DIR}/docs/screenshots/volume_animation.gif" >/dev/null 2>&1 || true

# Extract keyframes
total_crops=$(ls -1 /tmp/vol_crops/crop_*.png 2>/dev/null | wc -l)
if [[ $total_crops -ge 16 ]]; then
    # Pick closed (1), expanding (~4), mid (~7), open (15), closing (~20), closed (~28)
    cp /tmp/vol_crops/crop_001.png "${ROOT_DIR}/docs/screenshots/vol_anim_1_closed.png" 2>/dev/null || true
    cp /tmp/vol_crops/crop_004.png "${ROOT_DIR}/docs/screenshots/vol_anim_2_opening_25pct.png" 2>/dev/null || true
    cp /tmp/vol_crops/crop_007.png "${ROOT_DIR}/docs/screenshots/vol_anim_3_opening_60pct.png" 2>/dev/null || true
    cp /tmp/vol_crops/crop_015.png "${ROOT_DIR}/docs/screenshots/vol_anim_4_fully_open.png" 2>/dev/null || true
    cp /tmp/vol_crops/crop_020.png "${ROOT_DIR}/docs/screenshots/vol_anim_5_closing_50pct.png" 2>/dev/null || true
    cp /tmp/vol_crops/crop_028.png "${ROOT_DIR}/docs/screenshots/vol_anim_6_closed_again.png" 2>/dev/null || true

    ffmpeg -y \
        -i "${ROOT_DIR}/docs/screenshots/vol_anim_1_closed.png" \
        -i "${ROOT_DIR}/docs/screenshots/vol_anim_2_opening_25pct.png" \
        -i "${ROOT_DIR}/docs/screenshots/vol_anim_3_opening_60pct.png" \
        -i "${ROOT_DIR}/docs/screenshots/vol_anim_4_fully_open.png" \
        -i "${ROOT_DIR}/docs/screenshots/vol_anim_5_closing_50pct.png" \
        -i "${ROOT_DIR}/docs/screenshots/vol_anim_6_closed_again.png" \
        -filter_complex "[0:v][1:v][2:v][3:v][4:v][5:v]hstack=inputs=6" \
        "${ROOT_DIR}/docs/screenshots/volume_animation_strip.png" >/dev/null 2>&1 || true
    echo "Generated docs/screenshots/volume_animation_strip.png"
fi

# Generate close up of card with long title
ffmpeg -y -i "${ROOT_DIR}/docs/screenshots/01_home_featured.png" -vf "crop=220:140:465:440" "${ROOT_DIR}/docs/screenshots/10_card_long_title_closeup.png" >/dev/null 2>&1
echo "Captured docs/screenshots/10_card_long_title_closeup.png"

# Generate close up of player controls bar (Rewind -> Play -> Forward -> Favorite -> Shuffle -> Time -> Volume -> More -> Fullscreen)
ffmpeg -y -i "${ROOT_DIR}/docs/screenshots/08c_player_controls_bar.png" -vf "crop=1180:96:0:694" "${ROOT_DIR}/docs/screenshots/08d_player_controls_bar_closeup.png" >/dev/null 2>&1
echo "Captured docs/screenshots/08d_player_controls_bar_closeup.png"

# Kill Pass 1 Xvfb
kill "${XVFB_PID}" 2>/dev/null || true
sleep 1

# ==========================================
# PASS 2: Compact Resolution (960x640)
# ==========================================
echo "==> Starting Pass 2: 960x640 Compact Mode..."
rm -f /tmp/ah_demo_step_*

Xvfb :98 -screen 0 960x640x24 &
XVFB_PID2=$!
sleep 2

DISPLAY=:98 AH_WINDOW_SIZE=960x640 "${ROOT_DIR}/desktop/target/debug/adult-hub-desktop" --demo-capture &
APP_PID2=$!

wait_for_step "/tmp/ah_demo_step_1" "docs/screenshots/11_960x640_home.png" ":98"
wait_for_step "/tmp/ah_demo_step_3" "docs/screenshots/12_960x640_item_details.png" ":98"
wait_for_step "/tmp/ah_demo_step_4" "docs/screenshots/13_960x640_performer_sage_roux.png" ":98"
wait_for_step "/tmp/ah_demo_step_8" "docs/screenshots/14_960x640_player_volume.png" ":98"

# Terminate Pass 2
kill "${APP_PID2}" 2>/dev/null || true
kill "${XVFB_PID2}" 2>/dev/null || true

# Generate demo slideshow mp4 from captured frames
ffmpeg -y -framerate 0.5 -pattern_type glob -i "${ROOT_DIR}/docs/screenshots/0*.png" -c:v libx264 -r 25 -pix_fmt yuv420p "${ROOT_DIR}/docs/screenshots/adult_hub_demo.mp4" >/dev/null 2>&1 || true
echo "Generated docs/screenshots/adult_hub_demo.mp4"

echo "All screenshots and demo video captured successfully!"

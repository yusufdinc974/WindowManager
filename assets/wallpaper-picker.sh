#!/usr/bin/env bash
# ============================================================
#  lumie — Theme-Aware Wallpaper Picker
#  Usage: wallpaper-picker.sh <theme_name>
# ============================================================

set -euo pipefail

# ── Resolve real HOME (handles sandboxed sessions) ──
resolve_real_home() {
    local h="$HOME"
    if [[ "$h" =~ /run/user/[0-9]+/lumie-[^/]+/home ]]; then
        local real_home
        real_home="$(getent passwd "$(id -u)" | cut -d: -f6)"
        if [[ -n "$real_home" ]]; then
            echo "$real_home"
            return
        fi
    fi
    echo "$h"
}

REAL_HOME="$(resolve_real_home)"

THEME_NAME="${1:-}"
WALLPAPER_DIR="${REAL_HOME}/.config/lumie/wallpapers/${THEME_NAME}"
CACHE_DIR="${REAL_HOME}/.cache/lumie"
CACHE_FILE="${CACHE_DIR}/current_wallpaper.txt"
THEME_CACHE_FILE="${CACHE_DIR}/wallpaper_${THEME_NAME}.txt"

# ── Validate arguments ──
if [[ -z "$THEME_NAME" ]]; then
    echo "wallpaper-picker: error — no theme name provided" >&2
    exit 1
fi

if [[ ! -d "$WALLPAPER_DIR" ]]; then
    echo "wallpaper-picker: error — directory not found: $WALLPAPER_DIR" >&2
    exit 1
fi

# ── Collect image files ──
mapfile -t images < <(
    find "$WALLPAPER_DIR" -maxdepth 1 -type f \
        \( -iname '*.png' -o -iname '*.jpg' -o -iname '*.jpeg' \
           -o -iname '*.webp' -o -iname '*.bmp' -o -iname '*.gif' \) \
        -printf '%f\n' | sort
)

if [[ ${#images[@]} -eq 0 ]]; then
    echo "wallpaper-picker: no wallpapers found in $WALLPAPER_DIR" >&2
    exit 1
fi

# ── Present fuzzel menu ──
selected=$(printf '%s\n' "${images[@]}" | fuzzel -d -p "Wallpaper (${THEME_NAME}): ")

# ── Handle cancellation (empty selection) ──
if [[ -z "$selected" ]]; then
    echo "wallpaper-picker: selection cancelled"
    exit 0
fi

FULL_PATH="${WALLPAPER_DIR}/${selected}"

# ── Validate selected file exists ──
if [[ ! -f "$FULL_PATH" ]]; then
    echo "wallpaper-picker: error — file not found: $FULL_PATH" >&2
    exit 1
fi

# ── Apply wallpaper ──
pkill -x swaybg 2>/dev/null || true
sleep 0.1
swaybg -i "$FULL_PATH" -m fill &
disown

# ── Cache the selection ──
mkdir -p "$CACHE_DIR"
echo "$FULL_PATH" > "$CACHE_FILE"
echo "$FULL_PATH" > "$THEME_CACHE_FILE"

echo "wallpaper-picker: applied $FULL_PATH"
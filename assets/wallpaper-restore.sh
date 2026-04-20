#!/usr/bin/env bash
# ============================================================
#  lumie — Wallpaper Restore on Boot / Reload
#  Usage: wallpaper-restore.sh <theme_name>
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

apply_wallpaper() {
    local img="\$1"
    if [[ ! -f "$img" ]]; then
        return 1
    fi
    pkill -x swaybg 2>/dev/null || true
    sleep 0.1
    swaybg -i "$img" -m fill &
    disown
    mkdir -p "$CACHE_DIR"
    echo "$img" > "$CACHE_FILE"
    echo "$img" > "$THEME_CACHE_FILE"
    echo "wallpaper-restore: applied $img"
    return 0
}

# ── No theme name? Just try global cache ──
if [[ -z "$THEME_NAME" ]]; then
    if [[ -f "$CACHE_FILE" ]]; then
        cached=$(cat "$CACHE_FILE")
        if apply_wallpaper "$cached"; then
            exit 0
        fi
    fi
    echo "wallpaper-restore: no theme and no cache — nothing to apply" >&2
    exit 0
fi

# ── 1. Per-theme cache ──
if [[ -f "$THEME_CACHE_FILE" ]]; then
    cached=$(cat "$THEME_CACHE_FILE")
    if apply_wallpaper "$cached"; then
        exit 0
    fi
    echo "wallpaper-restore: per-theme cache stale, falling through"
fi

# ── 2. Global cache (only if it belongs to current theme dir) ──
if [[ -f "$CACHE_FILE" ]]; then
    cached=$(cat "$CACHE_FILE")
    case "$cached" in
        "${WALLPAPER_DIR}/"*)
            if apply_wallpaper "$cached"; then
                exit 0
            fi
            ;;
    esac
fi

# ── 3. Random pick from the theme wallpaper directory ──
if [[ -d "$WALLPAPER_DIR" ]]; then
    mapfile -t images < <(
        find "$WALLPAPER_DIR" -maxdepth 1 -type f \
            \( -iname '*.png' -o -iname '*.jpg' -o -iname '*.jpeg' \
               -o -iname '*.webp' -o -iname '*.bmp' -o -iname '*.gif' \) \
            -printf '%p\n'
    )

    if [[ ${#images[@]} -gt 0 ]]; then
        random_idx=$(( RANDOM % ${#images[@]} ))
        random_pick="${images[$random_idx]}"
        if apply_wallpaper "$random_pick"; then
            exit 0
        fi
    fi
fi

echo "wallpaper-restore: no wallpapers available for theme '$THEME_NAME'" >&2
exit 0
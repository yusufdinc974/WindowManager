#!/bin/bash
# mywm-opacity.sh — Long-running Waybar module for opacity display
trap 'exit 0' PIPE TERM INT

OPACITY_FILE="${XDG_RUNTIME_DIR:-/tmp}/mywm-opacity.json"

# Output initial state
if [ -f "$OPACITY_FILE" ]; then
    cat "$OPACITY_FILE"
else
    echo '{"text": "100%", "tooltip": "Window Opacity: 100%\nScroll to adjust • Click to reset", "class": "opacity", "percentage": 100}'
fi

# Watch for changes and re-output
while true; do
    if command -v inotifywait >/dev/null 2>&1; then
        inotifywait -qq -e close_write -e moved_to "$(dirname "$OPACITY_FILE")" 2>/dev/null
    else
        sleep 1
    fi

    if [ -f "$OPACITY_FILE" ]; then
        cat "$OPACITY_FILE" 2>/dev/null || true
    fi
done
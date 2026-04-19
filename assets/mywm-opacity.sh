#!/bin/bash
# mywm-opacity.sh — Long-running Waybar module for opacity display
trap 'exit 0' PIPE TERM INT

OPACITY_FILE="${XDG_RUNTIME_DIR:-/tmp}/mywm-opacity.json"
DEFAULT='{"text":"100%","tooltip":"Window Opacity: 100%\nClick: slider | Scroll: adjust | Right-click: reset","class":"opacity","percentage":100}'

# MUST output something immediately or Waybar hides the module
if [ -f "$OPACITY_FILE" ]; then
    cat "$OPACITY_FILE" 2>/dev/null || echo "$DEFAULT"
else
    echo "$DEFAULT"
fi

# Then watch for changes
while true; do
    if command -v inotifywait >/dev/null 2>&1; then
        inotifywait -qq -e close_write -e moved_to "$(dirname "$OPACITY_FILE")" 2>/dev/null
    else
        sleep 1
    fi

    if [ -f "$OPACITY_FILE" ]; then
        cat "$OPACITY_FILE" 2>/dev/null || echo "$DEFAULT"
    else
        echo "$DEFAULT"
    fi
done
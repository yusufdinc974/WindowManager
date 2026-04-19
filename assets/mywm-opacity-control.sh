#!/bin/bash
# mywm-opacity-control.sh — Visual opacity slider for Waybar
SOCK="/tmp/mywm.sock"

# Read current opacity
CURRENT=100
OPACITY_FILE="${XDG_RUNTIME_DIR:-/tmp}/mywm-opacity.json"
if [ -f "$OPACITY_FILE" ] && command -v python3 >/dev/null 2>&1; then
    CURRENT=$(python3 -c "
import json
try:
    d = json.load(open('$OPACITY_FILE'))
    print(d.get('percentage', 100))
except:
    print(100)
" 2>/dev/null)
fi
# Ensure CURRENT is a number
case "$CURRENT" in
    ''|*[!0-9]*) CURRENT=100 ;;
esac

# Build menu entries
ENTRIES=""
for pct in 100 95 90 85 80 75 70 65 60 55 50 40 30 20; do
    filled=$((pct / 5))
    empty=$((20 - filled))
    bar=""
    i=0; while [ $i -lt $filled ]; do bar="${bar}█"; i=$((i+1)); done
    i=0; while [ $i -lt $empty ]; do bar="${bar}░"; i=$((i+1)); done
    if [ "$pct" = "$CURRENT" ]; then
        ENTRIES="${ENTRIES}${bar}  ${pct}% ◄
"
    else
        ENTRIES="${ENTRIES}${bar}  ${pct}%
"
    fi
done

# Launch picker
CHOICE=""
if command -v fuzzel >/dev/null 2>&1; then
    CHOICE=$(printf '%s' "$ENTRIES" | fuzzel --dmenu --prompt "Opacity: " --width 35 --lines 14 --config /dev/null 2>/dev/null)
elif command -v wofi >/dev/null 2>&1; then
    CHOICE=$(printf '%s' "$ENTRIES" | wofi --dmenu --prompt "Opacity:" --width 380 --height 480 --cache-file /dev/null 2>/dev/null)
elif command -v rofi >/dev/null 2>&1; then
    CHOICE=$(printf '%s' "$ENTRIES" | rofi -dmenu -p "Opacity:" -no-config 2>/dev/null)
elif command -v bemenu >/dev/null 2>&1; then
    CHOICE=$(printf '%s' "$ENTRIES" | bemenu --prompt "Opacity:" 2>/dev/null)
elif command -v dmenu >/dev/null 2>&1; then
    CHOICE=$(printf '%s' "$ENTRIES" | dmenu -p "Opacity:" 2>/dev/null)
fi

# Parse and send
if [ -n "$CHOICE" ]; then
    PCT=$(echo "$CHOICE" | grep -oE '[0-9]+%' | head -1 | tr -d '%')
    if [ -n "$PCT" ] && [ "$PCT" -ge 10 ] 2>/dev/null && [ "$PCT" -le 100 ] 2>/dev/null; then
        python3 <<PYEOF
import socket, json
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("$SOCK")
msg = json.dumps({"SetOpacity": {"value": $PCT / 100}})
s.sendall(msg.encode())
s.close()
print(f"Sent opacity: {$PCT}%")
PYEOF
    fi
fi